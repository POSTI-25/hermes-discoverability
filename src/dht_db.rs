use std::{borrow::Cow, collections::HashMap, error::Error, iter, str::FromStr, time::Duration};

use clap::Parser;
use futures::{executor::block_on, future::FutureExt, stream::StreamExt};
use libp2p::{
    core::multiaddr::{Multiaddr, Protocol},
    dcutr, identify, identity, noise, ping, relay,
    kad::{self, store::{RecordStore, Error as StoreError}, Config, ProviderRecord, Record, RecordKey, QueryId},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, PeerId, StreamProtocol,
};
use tokio::{
    io::{self, AsyncBufReadExt},
    select,
};
use tracing::warn;
use tracing_subscriber::EnvFilter;

mod routing_table;
#[derive(Debug, Clone)]
pub struct SimpleSledStore {
    db: sled::Db,
}

impl SimpleSledStore {
    pub fn new(path: &str) -> Self {
        let db = sled::open(path).expect("Failed to open sled database");
        println!("Attempting to open database at: {}", path);

        println!("--- DHT Database Contents ---");
        let mut count = 0;
        for item in db.iter() {
            if let Ok((key_bytes, value_bytes)) = item {
                println!("[Record {}]", count + 1);
                println!("  Raw Key  : {}", String::from_utf8_lossy(&key_bytes));
                println!("  Value    : {}", String::from_utf8_lossy(&value_bytes));
                count += 1;
            }
        }
        println!("--- End of Database ({} records) ---", count);

        SimpleSledStore { db }
    }
}

impl RecordStore for SimpleSledStore {
    type RecordsIter<'a> = Box<dyn Iterator<Item = Cow<'a, Record>> + 'a>;
    type ProvidedIter<'a> = Box<dyn Iterator<Item = Cow<'a, ProviderRecord>> + 'a>;

    fn get(&self, key: &RecordKey) -> Option<Cow<Record>> {
        if let Ok(Some(value_bytes)) = self.db.get(key.as_ref()) {
            let record = Record {
                key: key.clone(),
                value: value_bytes.to_vec(),
                publisher: None,
                expires: None, 
            };
            return Some(Cow::Owned(record))
        } else {
            None
        }
    }

    fn put(&mut self, record: Record) -> Result<(), StoreError> {
        self.db.insert(record.key.as_ref(), record.value)
            // IMPORTANT: Flush the database to ensure the record is written to disk.
            .and_then(|_| self.db.flush().map_err(|e| sled::Error::from(e)))
            .map(|_| ())
            .map_err(|e| {
                warn!("Failed to put record to DB: {:?}", e);
                StoreError::ValueTooLarge
            })
    }

    fn remove(&mut self, key: &RecordKey) {
        let _ = self.db.remove(key.as_ref());
    }

    fn records(&self) -> Self::RecordsIter<'_> {
        Box::new(self.db.iter().filter_map(|res| {
            if let Ok((key_bytes, value_bytes)) = res {
                Some(Cow::Owned(Record {
                    key: RecordKey::new(&key_bytes),
                    value: value_bytes.to_vec(),
                    publisher: None,
                    expires: None,
                }))
            } else {
                None
            }
        }))
    }

    fn add_provider(&mut self, _record: ProviderRecord) -> Result<(), StoreError> { Ok(()) }
    fn providers(&self, _key: &RecordKey) -> Vec<ProviderRecord> { Vec::new() }
    fn provided(&self) -> Self::ProvidedIter<'_> { Box::new(iter::empty()) }
    fn remove_provider(&mut self, _key: &RecordKey, _provider: &PeerId) {}
}

#[derive(Debug, Parser)]
#[command(name = "libp2p DCUtR client with Kademlia")]
struct Opts {
    #[arg(long)]
    mode: Mode,
    #[arg(long)]
    secret_key_seed: u8,
    #[arg(long)]
    relay_address: Multiaddr,
    #[arg(long)]
    remote_peer_id: Option<PeerId>,
    // Optional address of a bootstrap node to connect to. //(A D H E E S H GAVE ADDRESS)
    #[arg(long)]
    bootstrap_node: Option<Multiaddr>,
}

#[derive(Clone, Debug, PartialEq, Parser)]
enum Mode { Dial, Listen }
impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dial" => Ok(Mode::Dial),
            "listen" => Ok(Mode::Listen),
            _ => Err("Expected 'dial' or 'listen'".to_string()),
        }
    }
}

const KAD_PROTOCOL_NAME: &str = "/hermes/kad/1.0.0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let opts = Opts::parse();

    #[derive(NetworkBehaviour)]
    struct Behaviour {
        relay_client: relay::client::Behaviour,
        ping: ping::Behaviour,
        identify: identify::Behaviour,
        dcutr: dcutr::Behaviour,
        kademlia: kad::Behaviour<SimpleSledStore>,
    }

    let local_key = generate_ed25519(opts.secret_key_seed);
    let db_path = format!("dht-database-{}", opts.secret_key_seed);
    let store = SimpleSledStore::new(&db_path);
    let kad_config = kad::Config::new(StreamProtocol::new(KAD_PROTOCOL_NAME));

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key.clone())
        .with_tokio()
        .with_tcp(tcp::Config::default().nodelay(true), noise::Config::new, yamux::Config::default)?
        .with_quic()
        .with_dns()?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|keypair, relay_behaviour| Behaviour {
            relay_client: relay_behaviour,
            ping: ping::Behaviour::new(ping::Config::new()),
            identify: identify::Behaviour::new(identify::Config::new(
                "/hermes/0.0.1".to_string(),
                keypair.public(),
            )),
            dcutr: dcutr::Behaviour::new(keypair.public().to_peer_id()),
            kademlia: kad::Behaviour::with_config(keypair.public().to_peer_id(), store.clone(), kad_config),
        })?
        .build();

        // A D D E D R O U T I N G T A B L E
    let mut routing_table = routing_table::RoutingTable::new(*swarm.local_peer_id());
    // A map to track which peers we are trying to dial by name. The value is the query ID.
    let mut pending_dials = HashMap::new();

    swarm.behaviour_mut().kademlia.set_mode(Some(kad::Mode::Server));
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;



        // S K I P P E D  S O M E  P A R T - N O T E 2



    // Connect to the relay server
    swarm.dial(opts.relay_address.clone()).unwrap();

    // If a bootstrap node is provided, add its address to Kademlia and bootstrap
    if let Some(bootstrap_addr) = opts.bootstrap_node {
        let Some(Protocol::P2p(peer_id)) = bootstrap_addr.iter().last() else {
            return Err("Expected bootstrap node address to contain P2p protocol".into());
        };
        swarm.behaviour_mut().kademlia.add_address(&peer_id, bootstrap_addr);
        if let Err(e) = swarm.behaviour_mut().kademlia.bootstrap() {
            tracing::warn!("Failed to bootstrap DHT: {:?}", e);
        }
    }
    
    // The rest of the setup logic... (omitted for brevity, no changes here)
    // ...
    
    println!("\nSwarm setup complete. Ready for commands (GET <key>, PUT <key> <value>, DIAL <peer_name>, PRINT_RT).");

    let mut stdin = io::BufReader::new(io::stdin()).lines();

    loop {
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                handle_input_line(&mut swarm.behaviour_mut().kademlia, &mut pending_dials, &routing_table, line);
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Listening on address: {address}");
                }
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    tracing::info!(peer=%peer_id, ?endpoint, "Established new connection");
                    swarm.behaviour_mut().kademlia.add_address(&peer_id, endpoint.get_remote_address().clone());
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => {
                    routing_table.handle_kad_event(&event);

                    if let kad::Event::OutboundQueryProgressed { id, result, .. } = event {
                         match result {
                            kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(
                                kad::PeerRecord { record, .. },
                            ))) => {
                                let key_str = String::from_utf8_lossy(record.key.as_ref()).to_string();
                                println!(
                                    "Got Record: key='{}', value='{}'",
                                    key_str,
                                    String::from_utf8_lossy(&record.value),
                                );

                                // Check if this record was for a pending dial
                                if pending_dials.remove(&key_str).is_some() {
                                    println!("Record is for a pending dial. Attempting to connect...");
                                    let value_str = String::from_utf8_lossy(&record.value);
                                    let parts: Vec<&str> = value_str.split(':').collect();
                                    if parts.len() == 2 {
                                        let peer_id_str = parts[0];
                                        let addr_str = parts[1];
                                        if let (Ok(peer_id), Ok(addr)) = (PeerId::from_str(peer_id_str), Multiaddr::from_str(addr_str)) {
                                            println!("Dialing peer {} at address {}", peer_id, addr);
                                            swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                                            if let Err(e) = swarm.dial(addr) {
                                                eprintln!("Failed to dial peer {}: {:?}", peer_id, e);
                                            }
                                        } else {
                                            eprintln!("Failed to parse peer ID or multiaddress from record value.");
                                        }
                                    } else {
                                        eprintln!("Invalid record format for dialing. Expected '<peer_id>:<multiaddr>'.");
                                    }
                                }
                            }
                            kad::QueryResult::GetRecord(Err(err)) => {
                                eprintln!("Failed to get record: {err:?}");
                                // If a pending dial fails, we should probably remove it.
                                // This requires mapping query ID back to key, which is complex here.
                                // For simplicity, we leave it, but a real app would need better tracking.
                            }
                            kad::QueryResult::PutRecord(Ok(kad::PutRecordOk { key })) => {
                                println!(
                                    "Successfully put record with key: '{}'",
                                    String::from_utf8_lossy(key.as_ref())
                                );
                            }
                            // ... other Kademlia event handlers
                             _ => {}
                        }
                    }
                },
                _ => {}
            }
        }
    }
}

fn handle_input_line(
    kademlia: &mut kad::Behaviour<SimpleSledStore>,
    pending_dials: &mut HashMap<String, QueryId>,
    routing_table: &routing_table::RoutingTable,
    line: String,
) {
    let mut args = line.split_whitespace();
    match args.next() {
        Some("GET") => {
            if let Some(key) = args.next() {
                kademlia.get_record(kad::RecordKey::new(&key));
            } else {
                eprintln!("Usage: GET <key>");
            }
        }
        Some("PUT") => {
            // ... (same as before, but let's make it register the node's multiaddress)
            if let (Some(key), Some(value)) = (args.next(), args.next()) {
                let record = kad::Record {
                    key: kad::RecordKey::new(&key.as_bytes()),
                    value: value.as_bytes().to_vec(),
                    publisher: None,
                    expires: None,
                };
                if let Err(e) = kademlia.put_record(record, kad::Quorum::One) {
                    eprintln!("Failed to put record: {:?}", e);
                }
            } else {
                eprintln!("Usage: PUT <key> <value>");
            }
        }
        Some("DIAL") => {
            if let Some(peer_name) = args.next() {
                println!("Searching for peer '{}' in the DHT...", peer_name);
                let key = kad::RecordKey::new(&peer_name);
                let query_id = kademlia.get_record(key);
                pending_dials.insert(peer_name.to_string(), query_id);
            } else {
                eprintln!("Usage: DIAL <peer_name>");
            }
        }
        Some("PRINT_RT") => {
            routing_table.print();
        }
        _ => {
            eprintln!("expected GET, PUT, DIAL, or PRINT_RT");
        }
    }
}

fn generate_ed25519(secret_key_seed: u8) -> identity::Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = secret_key_seed;
    identity::Keypair::ed25519_from_bytes(bytes).expect("only errors on wrong length")
}