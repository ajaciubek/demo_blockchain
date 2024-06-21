use std::time::Duration;

use chrono::prelude::*;
use libp2p::{
    core::{transport, upgrade},
    futures::StreamExt,
    mplex,
    noise::{Keypair, NoiseConfig, X25519Spec},
    swarm::{Swarm, SwarmBuilder},
    tcp::TokioTcpConfig,
    Transport,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{stdin, AsyncBufReadExt, BufReader},
    select, spawn,
    sync::mpsc,
    time::sleep,
};
pub struct App {
    pub blocks: Vec<Block>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Block {
    pub id: u64,
    pub hash: String,
    pub previous_hash: String,
    pub timestamp: i64,
    pub data: String,
    pub nonce: u64,
}

mod p2p;

const DIFFICULTY_LEVEL: usize = 2;

fn validate_hash(hash: &[u8], difficulty: usize) -> bool {
    for i in 0..(difficulty / 2) {
        if hash[i] != 0 {
            return false;
        }
    }
    true
}

fn calculate_hash(id: u64, timestamp: i64, previous_hash: &str, data: &str, nonce: u64) -> Vec<u8> {
    let data_json = serde_json::json!({
    "id":id,
    "previous_hash":previous_hash,
    "data":data,
    "timestamp":timestamp,
    "nonce":nonce
    });
    let mut hasher = Sha256::new();
    hasher.update(data_json.to_string().as_bytes());
    hasher.finalize().as_slice().to_owned() // consider to_vec
}

fn mine_block(id: u64, timestamp: i64, previous_hash: &str, data: &str) -> (u64, String) {
    let mut nonce = 0;
    loop {
        if nonce % 1000 == 0 {
            println!("{}", nonce);
        }
        let hash = calculate_hash(id, timestamp, previous_hash, data, nonce);
        if validate_hash(&hash, DIFFICULTY_LEVEL) {
            return (nonce, hex::encode(hash));
        }
        nonce += 1;
    }
}
impl Block {
    fn new(id: u64, previous_hash: String, data: String) -> Self {
        let timestamp = Utc::now().timestamp();
        let (nonce, hash) = mine_block(id, timestamp, &previous_hash, &data);
        Self {
            id,
            hash,
            timestamp,
            previous_hash,
            data,
            nonce,
        }
    }
}

impl App {
    fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    fn choose_chain(&mut self, local: Vec<Block>, remote: Vec<Block>) -> Vec<Block> {
        let is_local_valid = self.is_chain_valid(&local);
        let is_remote_valid = self.is_chain_valid(&remote);

        if is_local_valid && is_remote_valid {
            if local.len() >= remote.len() {
                local
            } else {
                remote
            }
        } else if is_remote_valid && !is_local_valid {
            remote
        } else if is_local_valid && !is_remote_valid {
            local
        } else {
            panic!("local and remote are invalid");
        }
    }

    fn is_chain_valid(&self, chain: &[Block]) -> bool {
        for i in 1..chain.len() {
            let previous_block = chain.get(i - 1).expect("has to exist");
            let block = chain.get(i).expect("has to exist");
            if !self.is_block_valid(block, previous_block) {
                return false;
            }
        }
        true
    }

    fn genesis(&mut self) {
        let genesis_block = Block {
            id: 0,
            timestamp: Utc::now().timestamp(),
            previous_hash: String::from("genesis"),
            data: String::from("genesis!"),
            nonce: 2836,
            hash: "0000f816a87f806bb0073dcf026a64fb40c946b5abee2573702828694d5b4c43".to_string(),
        };
        self.blocks.push(genesis_block);
    }

    fn try_add_bock(&mut self, block: Block) {
        let latest_block = self.blocks.last().expect("no blocks added");
        if self.is_block_valid(&block, latest_block) {
            self.blocks.push(block);
        } else {
            panic!("could not add block - invalid")
        }
    }

    fn is_block_valid(&self, block: &Block, previous_block: &Block) -> bool {
        if block.previous_hash != previous_block.hash {
            return false;
        } else if !validate_hash(
            &hex::decode(&block.hash).expect("can decode from hex"),
            DIFFICULTY_LEVEL,
        ) {
            println!("Wrong difficulty prefix");
            return false;
        } else if block.id != previous_block.id + 1 {
            println!("Wrong id");
            return false;
        } else if hex::encode(calculate_hash(
            block.id,
            block.timestamp,
            &block.previous_hash,
            &block.data,
            block.nonce,
        )) != block.hash
        {
            println!("Wrong hash");
            return false;
        }
        true
    }
}
#[tokio::main]
async fn main() {
    let (response_sender, mut response_rcv) = mpsc::unbounded_channel();
    let (init_sender, mut init_rcv) = mpsc::unbounded_channel();
    let auth_keys = Keypair::<X25519Spec>::new()
        .into_authentic(&p2p::KEYS)
        .expect("can create auth keys");

    let transport = TokioTcpConfig::new()
        .upgrade(upgrade::Version::V1)
        .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed();
    let behaviour = p2p::AppBehaviour::new(App::new(), response_sender, init_sender.clone()).await;
    let mut swarm = SwarmBuilder::new(transport, behaviour, *p2p::PEER_ID)
        .executor(Box::new(|fut| {
            spawn(fut);
        }))
        .build();
    let mut stdin = BufReader::new(stdin()).lines();

    Swarm::listen_on(
        &mut swarm,
        "/ip4/0.0.0.0/tcp/0"
            .parse()
            .expect("can get a local socket"),
    )
    .expect("swarm can be started");

    spawn(async move {
        sleep(Duration::from_secs(1)).await;
        println!("sending init event");
        init_sender.send(true).expect("can send init event");
    });

    loop {
        let evt = {
            select! {
                line = stdin.next_line() => {
                    Some(p2p::EventType::Input(line.expect("can get line").expect("can read line from sdin")))
                },
                response = response_rcv.recv() =>{
                    Some(p2p::EventType::LocalChainResponse(response.expect("response exist")))
                },
                _init = init_rcv.recv()=>{
                    Some(p2p::EventType::Init)
                },
                event = swarm.select_next_some() =>{
                    print!("unhandled swarm event {:?}",event);
                    None
                }
            }
        };

        if let Some(event) = evt {
            match event {
                p2p::EventType::Init => {}
                p2p::EventType::LocalChainResponse(resp) => {}
                p2p::EventType::Input(line) => match line.as_str() {
                    "ls p" => {
                        p2p::handle_print_peers(&swarm);
                    }
                    _ => print!("unspported command"),
                },
            }
        }
    }
}
