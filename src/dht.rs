use libp2p::kad::{Behaviour, Record, RecordKey};
use libp2p::PeerId;
use libp2p::kad::Quorum;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::NetworkBehaviour; // Import the derive macro

#[derive(NetworkBehaviour)]
pub struct KademliaBehaviour {
    pub dht: Behaviour<MemoryStore>,
}

pub fn put_record(
    behaviour: &mut KademliaBehaviour,
    key: &str,
    value: Vec<u8>,
    peer_id: PeerId,
) -> Result<libp2p::kad::QueryId, String> {
    let record = Record {
        key: RecordKey::new(&key),
        value,
        publisher: Some(peer_id),
        expires: None, // Specifies that the record does not expire
    };

    match behaviour.dht.put_record(record, Quorum::One) {
        Ok(id) => Ok(id),
        Err(e) => Err(format!("Failed to put record: {}", e)),
    }
    /* A quick note: the put_record() function does not return "Ok" directly. Instead, it returns the query ID of the put operation, which can be used to track its progress. */
}

pub fn get_record(
    behaviour: &mut KademliaBehaviour,
    key: &str,
) -> libp2p::kad::QueryId {
    behaviour.dht.get_record(RecordKey::new(&key))
}

pub fn remove_record(
    behaviour: &mut KademliaBehaviour,
    key: &str,
) -> Result<(), String> {
    behaviour.dht.remove_record(&RecordKey::new(&key));
    Ok(())
}

fn main() {
    // Placeholder main function
    println!("Hermes ACM");
}
