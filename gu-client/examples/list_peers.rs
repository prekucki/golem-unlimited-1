extern crate actix;
extern crate actix_web;
extern crate futures;
extern crate gu_client;
extern crate gu_net;

use actix::Arbiter;
use futures::{future, Future};
use gu_client::async::Driver;

fn main() {
    let driver = Driver::from_addr("127.0.0.1:61622");
    actix::System::run(move || {
        Arbiter::spawn(
            driver
                .list_peers()
                .and_then(|peers| {
                    peers.for_each(|peer| println!("peer_id={:#?}", peer.node_id));
                    future::ok(())
                }).map_err(|e| {
                    println!("Error while listing peers: {:#?}.", e);
                    ()
                }).then(|_| future::ok(actix::System::current().stop())),
        );
    });
}
