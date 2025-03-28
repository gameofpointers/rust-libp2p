// Copyright 2020 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use futures::{channel::oneshot, prelude::*, ready};
use libp2p_core::{
    multiaddr::Protocol,
    muxing::StreamMuxerBox,
    transport::{self, MemoryTransport},
    upgrade, Multiaddr, Transport,
};
use libp2p_identity as identity;
use libp2p_identity::PeerId;
use libp2p_plaintext::PlainText2Config;
use libp2p_swarm::{dummy, SwarmBuilder, SwarmEvent};
use rand::random;
use std::task::Poll;

type TestTransport = transport::Boxed<(PeerId, StreamMuxerBox)>;

fn mk_transport(up: upgrade::Version) -> (PeerId, TestTransport) {
    let keys = identity::Keypair::generate_ed25519();
    let id = keys.public().to_peer_id();
    (
        id,
        MemoryTransport::default()
            .upgrade(up)
            .authenticate(PlainText2Config {
                local_public_key: keys.public(),
            })
            .multiplex(libp2p_yamux::Config::default())
            .boxed(),
    )
}

/// Tests the transport upgrade process with all supported
/// upgrade protocol versions.
#[test]
fn transport_upgrade() {
    let _ = env_logger::try_init();

    fn run(up: upgrade::Version) {
        let (dialer_id, dialer_transport) = mk_transport(up);
        let (listener_id, listener_transport) = mk_transport(up);

        let listen_addr = Multiaddr::from(Protocol::Memory(random::<u64>()));

        let mut dialer =
            SwarmBuilder::with_async_std_executor(dialer_transport, dummy::Behaviour, dialer_id)
                .build();
        let mut listener = SwarmBuilder::with_async_std_executor(
            listener_transport,
            dummy::Behaviour,
            listener_id,
        )
        .build();

        listener.listen_on(listen_addr).unwrap();
        let (addr_sender, addr_receiver) = oneshot::channel();

        let client = async move {
            let addr = addr_receiver.await.unwrap();
            dialer.dial(addr).unwrap();
            futures::future::poll_fn(move |cx| loop {
                if let SwarmEvent::ConnectionEstablished { .. } =
                    ready!(dialer.poll_next_unpin(cx)).unwrap()
                {
                    return Poll::Ready(());
                }
            })
            .await
        };

        let mut addr_sender = Some(addr_sender);
        let server = futures::future::poll_fn(move |cx| loop {
            match ready!(listener.poll_next_unpin(cx)).unwrap() {
                SwarmEvent::NewListenAddr { address, .. } => {
                    addr_sender.take().unwrap().send(address).unwrap();
                }
                SwarmEvent::IncomingConnection { .. } => {}
                SwarmEvent::ConnectionEstablished { .. } => return Poll::Ready(()),
                _ => {}
            }
        });

        async_std::task::block_on(future::select(Box::pin(server), Box::pin(client)));
    }

    run(upgrade::Version::V1);
    run(upgrade::Version::V1Lazy);
}
