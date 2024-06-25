use std::time::Duration;

use libp2p::{
    core::upgrade,
    futures::StreamExt,
    mplex,
    noise::{Keypair, NoiseConfig, X25519Spec},
    swarm::{Swarm, SwarmBuilder},
    tcp::TokioTcpConfig,
    Transport,
};
use p2p::PEER_ID;
use tokio::{
    io::{stdin, AsyncBufReadExt, BufReader},
    select, spawn,
    sync::mpsc,
    time::sleep,
};

mod blockchain;
mod p2p;

#[tokio::main]
async fn main() {
    println!("PEER ID {}", *PEER_ID);
    let (response_sender, mut response_rcv) = mpsc::unbounded_channel();
    let (init_sender, mut init_rcv) = mpsc::unbounded_channel();
    // this will keep the channel open so recv will sleep
    let _init_sender = init_sender.clone();

    let auth_keys = Keypair::<X25519Spec>::new()
        .into_authentic(&p2p::KEYS)
        .expect("can create auth keys");

    let transport = TokioTcpConfig::new()
        .upgrade(upgrade::Version::V1)
        .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed();
    let behaviour = p2p::AppBehaviour::new(blockchain::App::new(), response_sender).await;
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
                _event = swarm.select_next_some() =>{
                    None
                }
            }
        };

        if let Some(event) = evt {
            match event {
                p2p::EventType::Init => {
                    swarm.behaviour_mut().handle_init();
                }
                p2p::EventType::LocalChainResponse(resp) => {
                    let json = serde_json::to_string(&resp).expect("can jsonify response");
                    swarm
                        .behaviour_mut()
                        .floodsub
                        .publish(p2p::CHAIN_TOPIC.clone(), json.as_bytes());
                }
                p2p::EventType::Input(line) => match line.as_str() {
                    "ls p" => swarm.behaviour().handle_print_peers(),
                    cmd if cmd.starts_with("create b") => {
                        swarm.behaviour_mut().handle_create_block(cmd)
                    }
                    cmd if cmd.starts_with("ls c") => swarm.behaviour().print_chain(),
                    _ => print!("unspported command"),
                },
            }
        }
    }
}
