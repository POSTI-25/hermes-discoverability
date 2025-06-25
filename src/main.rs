use libp2p::PeerId;

fn main() {
    println!("Hermes ACM");

    let peer_id = PeerId::random(); // simulate other user's peer ID
    put_rec::put_record_global("samarth-seed-007", peer_id);
} 
