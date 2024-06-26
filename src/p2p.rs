use std::collections::HashSet;

use crate::blockchain::{App, Block};
use libp2p::{
    floodsub::{Floodsub, FloodsubEvent, Topic},
    identity,
    mdns::{Mdns, MdnsEvent},
    swarm::NetworkBehaviourEventProcess,
    NetworkBehaviour, PeerId,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub static KEYS: Lazy<identity::Keypair> = Lazy::new(identity::Keypair::generate_ed25519);
pub static PEER_ID: Lazy<PeerId> = Lazy::new(|| PeerId::from(KEYS.public()));
pub static CHAIN_TOPIC: Lazy<Topic> = Lazy::new(|| Topic::new("chains"));
pub static BLOCK_TOPIC: Lazy<Topic> = Lazy::new(|| Topic::new("blocks"));

#[derive(Debug, Serialize, Deserialize)]
pub struct ChainResponse {
    pub blocks: Vec<Block>,
    pub receiver: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalChainRequest {
    pub from_peer_id: String,
}

pub enum EventType {
    LocalChainResponse(ChainResponse),
    Input(String),
    Init,
}

#[derive(NetworkBehaviour)]
pub struct AppBehaviour {
    pub floodsub: Floodsub,
    pub mdns: Mdns,
    #[behaviour(ignore)]
    pub reponse_sender: mpsc::UnboundedSender<ChainResponse>,
    #[behaviour(ignore)]
    pub app: App,
}

impl AppBehaviour {
    pub async fn new(app: App, reponse_sender: mpsc::UnboundedSender<ChainResponse>) -> Self {
        let mut behaviour = AppBehaviour {
            app,
            floodsub: Floodsub::new(*PEER_ID),
            mdns: Mdns::new(Default::default())
                .await
                .expect("cannot create mdns"),
            reponse_sender,
        };
        behaviour.floodsub.subscribe(CHAIN_TOPIC.clone());
        behaviour.floodsub.subscribe(BLOCK_TOPIC.clone());
        behaviour
    }

    pub fn handle_create_block(&mut self, cmd: &str) {
        if let Some(data) = cmd.strip_prefix("create b") {
            let latest_block = self.app.blocks.last().expect("there is at least one block");
            let block = Block::new(
                latest_block.id + 1,
                latest_block.hash.clone(),
                data.to_owned(),
            );
            let json = serde_json::to_string(&block).expect("can jsonify request");
            self.app.blocks.push(block);
            println!("broadcasting new block");
            self.floodsub.publish(BLOCK_TOPIC.clone(), json.as_bytes());
        }
    }

    pub fn print_chain(&self) {
        let json = serde_json::to_string_pretty(&self.app.blocks).expect("can jsonify blcoks");
        print!("{}", json);
    }

    pub fn handle_init(&mut self) {
        let peers = self.get_list_peers();
        self.app.genesis();

        println!("connected nodes: {}", peers.len());

        if !peers.is_empty() {
            let req = LocalChainRequest {
                from_peer_id: peers.iter().last().expect("at lease one peer").to_string(),
            };

            let json = serde_json::to_string(&req).expect("can jsonify request");

            self.floodsub.publish(CHAIN_TOPIC.clone(), json.as_bytes());
        }
    }

    fn get_list_peers(&self) -> Vec<String> {
        println!("Discover peers");
        let nodes = self.mdns.discovered_nodes();
        let mut unique_peers = HashSet::new();
        for peer in nodes {
            unique_peers.insert(peer);
        }
        unique_peers.iter().map(|p| p.to_string()).collect()
    }

    pub fn handle_print_peers(&self) {
        let peers = self.get_list_peers();
        peers.iter().for_each(|p| println!("{}", p));
    }
}

impl NetworkBehaviourEventProcess<MdnsEvent> for AppBehaviour {
    fn inject_event(&mut self, event: MdnsEvent) {
        match event {
            MdnsEvent::Discovered(discovered_list) => {
                for (peer, _addr) in discovered_list {
                    self.floodsub.add_node_to_partial_view(peer);
                }
            }
            MdnsEvent::Expired(expired_list) => {
                for (peer, _addr) in expired_list {
                    if !self.mdns.has_node(&peer) {
                        self.floodsub.remove_node_from_partial_view(&peer);
                    }
                }
            }
        }
    }
}

impl NetworkBehaviourEventProcess<FloodsubEvent> for AppBehaviour {
    fn inject_event(&mut self, event: FloodsubEvent) {
        if let FloodsubEvent::Message(msg) = event {
            if let Ok(resp) = serde_json::from_slice::<ChainResponse>(&msg.data) {
                if resp.receiver == PEER_ID.to_string() {
                    println!("response from {}:", msg.source.to_string());
                    resp.blocks.iter().for_each(|r| println!("{:?}", r));
                    self.app.blocks = self.app.choose_chain(self.app.blocks.clone(), resp.blocks);
                }
            } else if let Ok(resp) = serde_json::from_slice::<LocalChainRequest>(&msg.data) {
                println!("sending local chain to {}", msg.source.to_string());
                let peer_id = resp.from_peer_id;
                if peer_id == PEER_ID.to_string() {
                    if let Err(e) = self.reponse_sender.send(ChainResponse {
                        blocks: self.app.blocks.clone(),
                        receiver: msg.source.to_string(),
                    }) {
                        print!("Error sending reposnse {}", e);
                    }
                }
            } else if let Ok(block) = serde_json::from_slice::<Block>(&msg.data) {
                println!("received new block from {}", msg.source.to_string());
                self.app.try_add_bock(block);
            }
        }
    }
}
