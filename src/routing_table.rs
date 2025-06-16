use std::collections::VecDeque;
use libp2p::PeerId;

#[derive(Debug, Clone)]

pub struct PeerInfo{
    pub peer_id: PeerId,
    pub distance: Option<u64>, //xor-distance
    pub last_seen: std::time::Instant,
    pub latency: Option<std::time::Duration>,
    pub address: Option<libp2p::core::multiaddr::Multiaddr>,
    pub is_online: bool,
}

pub struct Bucket{
    pub peers: VecDeque<PeerInfo>,
    pub k: usize, // maximum peers in the bucket, default value is 20
}

impl Bucket {
    pub fn new(k: usize) -> Self {
        Self {
            peers: VecDeque::new(),
            k,
        }
    }
    pub fn add_peer(&mut self, new_peer: PeerInfo) {
        if let Some(pos) = self.peers.iter().position(|p| p.peer_id == new_peer.peer_id) {
            self.peers.remove(pos);
            self.peers.push_back(new_peer);
            return; // Update existing peer, this avoids duplicates.
        }
        if self.peers.len() >= self.k {
            if let Some(pos) = self.peers.iter().position(|p| !p.is_online) {
                self.peers.remove(pos);
            } else {
                self.peers.pop_front();
            } // Remove the oldest inactive peer if the bucket is full.
        }
        self.peers.push_back(new_peer);
    }

}

