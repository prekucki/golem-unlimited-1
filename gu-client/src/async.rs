use crate::error::Error;
use actix_web::{client, http, HttpMessage};
use bytes::Bytes;
use futures::{future, prelude::*};
use gu_actix::release::{AsyncRelease, Handle};
use gu_model::peers::PeerInfo;
use gu_model::{
    deployment::DeploymentInfo,
    envman,
    session::{self, BlobInfo, HubExistingSession, HubSessionSpec, Metadata},
};
use gu_net::rpc::peer::PeerSessionInfo;
use gu_net::types::NodeId;
use serde::de::DeserializeOwned;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use std::{env, str};
use url::Url;

/// Connection to a single hub.
#[derive(Clone, Debug)]
pub struct HubConnection {
    hub_connection_inner: Arc<HubConnectionInner>,
}

#[derive(Debug)]
struct HubConnectionInner {
    url: Url,
}

impl Default for HubConnection {
    fn default() -> Self {
        match env::var("GU_HUB_ADDR") {
            Ok(addr) => HubConnection::from_addr(addr).unwrap(),
            Err(_) => HubConnection::from_addr("127.0.0.1:61622").unwrap(),
        }
    }
}

impl HubConnection {
    /// creates a hub connection from a given address:port, e.g. 127.0.0.1:61621
    pub fn from_addr<T: Into<String>>(addr: T) -> Result<HubConnection, Error> {
        Url::parse(&format!("http://{}/", addr.into()))
            .map_err(Error::InvalidAddress)
            .map(|url| HubConnection {
                hub_connection_inner: Arc::new(HubConnectionInner { url: url }),
            })
    }
    /// creates a new hub session
    pub fn new_session(
        &self,
        session_info: HubSessionSpec,
    ) -> impl Future<Item = Handle<HubSession>, Error = Error> {
        let sessions_url = format!("{}sessions", self.hub_connection_inner.url);
        let request = match client::ClientRequest::post(sessions_url).json(session_info) {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        let hub_connection_for_session = self.clone();
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| {
                    if response.status() != http::StatusCode::CREATED {
                        return future::Either::A(future::err(Error::CannotCreateHubSession(
                            response.status(),
                        )));
                    }
                    future::Either::B(response.body().map_err(Error::CannotGetResponseBody))
                })
                .and_then(|body| {
                    future::ok(Handle::new(HubSession {
                        hub_connection: hub_connection_for_session,
                        session_id: match str::from_utf8(&body.to_vec()) {
                            Ok(str) => str.to_string(),
                            Err(e) => return future::err(Error::CannotConvertToUTF8(e)),
                        },
                    }))
                }),
        )
    }
    pub fn auth_app<T: Into<String>, U: Into<String>>(&self, _app_name: T, _token: Option<U>) {}
    /// returns all peers connected to the hub
    pub fn list_peers(&self) -> impl Future<Item = impl Iterator<Item = PeerInfo>, Error = Error> {
        let url = format!("{}peers", self.hub_connection_inner.url);
        match client::ClientRequest::get(url).finish() {
            Ok(r) => future::Either::A(
                r.send()
                    .map_err(Error::CannotSendRequest)
                    .and_then(|response| match response.status() {
                        http::StatusCode::OK => {
                            future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                        }
                        status => future::Either::B(future::err(Error::CannotListHubPeers(status))),
                    })
                    .and_then(|answer_json: Vec<PeerInfo>| future::ok(answer_json.into_iter())),
            ),
            Err(e) => future::Either::B(future::err(Error::CannotCreateRequest(e))),
        }
    }
    /// returns information about all hub sessions
    pub fn list_sessions(
        &self,
    ) -> impl Future<Item = impl Iterator<Item = HubExistingSession>, Error = Error> {
        let url = format!("{}sessions", self.hub_connection_inner.url);
        match client::ClientRequest::get(url).finish() {
            Ok(r) => future::Either::A(
                r.send()
                    .map_err(Error::CannotSendRequest)
                    .and_then(|response| match response.status() {
                        http::StatusCode::OK => {
                            future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                        }
                        status => {
                            future::Either::B(future::err(Error::CannotListHubSessions(status)))
                        }
                    })
                    .and_then(|answer_json: Vec<_>| future::ok(answer_json.into_iter())),
            ),
            Err(e) => future::Either::B(future::err(Error::CannotCreateRequest(e))),
        }
    }
    /// returns hub session object
    pub fn hub_session<T: Into<String>>(&self, session_id: T) -> HubSession {
        HubSession {
            hub_connection: self.clone(),
            session_id: session_id.into(),
        }
    }

    pub fn peer<T: Into<NodeId>>(&self, node_id: T) -> ProviderRef {
        let connection = self.clone();
        let node_id = node_id.into();

        ProviderRef {
            connection,
            node_id,
        }
    }

    fn url(&self) -> &str {
        self.hub_connection_inner.url.as_ref()
    }

    fn fetch_json<T: DeserializeOwned + 'static>(
        &self,
        url: &str,
    ) -> impl Future<Item = T, Error = Error> {
        client::ClientRequest::get(&url)
            .finish()
            .into_future()
            .map_err(Error::CannotCreateRequest)
            .and_then(|r| r.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::OK => Ok(response),
                status => Err(Error::CannotGetPeerInfo(status)),
            })
            .and_then(|response| response.json().map_err(Error::InvalidJSONResponse))
    }

    fn delete_resource(&self, url: &str) -> impl Future<Item = (), Error = Error> {
        client::ClientRequest::delete(&url)
            .finish()
            .into_future()
            .map_err(Error::CannotCreateRequest)
            .and_then(|r| r.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::NO_CONTENT => future::Either::A(future::ok(())),
                http::StatusCode::OK => future::Either::B(
                    response
                        .json()
                        .map_err(Error::InvalidJSONResponse)
                        .and_then(|j: serde_json::Value| Ok(eprintln!("{}", j))),
                ),
                http::StatusCode::NOT_FOUND => {
                    future::Either::A(future::err(Error::ResourceNotFound))
                }
                status => future::Either::A(future::err(Error::CannotGetPeerInfo(status))),
            })
    }
}

/// Hub session.
#[derive(Clone, Debug)]
pub struct HubSession {
    hub_connection: HubConnection,
    session_id: String,
}

impl HubSession {
    /// adds peers to the hub session
    pub fn add_peers<T, U>(&self, peers: T) -> impl Future<Item = Vec<NodeId>, Error = Error>
    where
        T: IntoIterator<Item = U>,
        U: AsRef<str>,
    {
        let add_url = format!(
            "{}sessions/{}/peers",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        let peer_vec: Vec<String> = peers.into_iter().map(|peer| peer.as_ref().into()).collect();
        let request = match client::ClientRequest::post(add_url).json(peer_vec) {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        let session_id_copy = self.session_id.clone();
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::NOT_FOUND => {
                        future::Either::A(future::err(Error::SessionNotFound(session_id_copy)))
                    }
                    http::StatusCode::INTERNAL_SERVER_ERROR => future::Either::A(future::err(
                        Error::CannotAddPeersToSession(response.status()),
                    )),
                    _ => future::Either::B(
                        response.json().map_err(|e| Error::InvalidJSONResponse(e)),
                    ),
                }),
        )
    }
    /// creates a new blob
    pub fn new_blob(&self) -> impl Future<Item = Blob, Error = Error> {
        let new_blob_url = format!(
            "{}sessions/{}/blobs",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        let request = match client::ClientRequest::post(new_blob_url).finish() {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        let hub_session_copy = self.clone();
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::CREATED => {
                        future::Either::A(response.body().map_err(Error::CannotGetResponseBody))
                    }
                    status => future::Either::B(future::err(Error::CannotCreateBlob(status))),
                })
                .and_then(|body| {
                    future::ok(Blob {
                        hub_session: hub_session_copy,
                        blob_id: match str::from_utf8(&body.to_vec()) {
                            Ok(str) => str.to_string(),
                            Err(e) => return future::err(Error::CannotConvertToUTF8(e)),
                        },
                    })
                }),
        )
    }
    /// gets single peer by its id
    pub fn peer(&self, node_id: NodeId) -> Peer {
        Peer {
            node_id: node_id,
            hub_session: self.clone(),
        }
    }
    /// gets single peer by its id given as a string
    pub fn peer_from_str<T: AsRef<str>>(&self, node_id: T) -> Result<Peer, Error> {
        Ok(self.peer(
            node_id
                .as_ref()
                .parse()
                .map_err(|_| Error::InvalidPeer(node_id.as_ref().to_string()))?,
        ))
    }

    /// returns all session peers
    pub fn list_peers(&self) -> impl Future<Item = impl Iterator<Item = PeerInfo>, Error = Error> {
        let url = format!(
            "{}sessions/{}/peers",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        let request = match client::ClientRequest::get(url).finish() {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::OK => {
                        future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                    }
                    status => future::Either::B(future::err(Error::CannotListSessionPeers(status))),
                })
                .and_then(|answer_json: Vec<PeerInfo>| future::ok(answer_json.into_iter())),
        )
    }
    /// gets single blob by its id
    pub fn blob<T: Into<String>>(&self, blob_id: T) -> Blob {
        Blob {
            blob_id: blob_id.into(),
            hub_session: self.clone(),
        }
    }
    /// returns all session blobs
    pub fn list_blobs(&self) -> impl Future<Item = impl Iterator<Item = BlobInfo>, Error = Error> {
        let url = format!(
            "{}sessions/{}/blobs",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        let request = match client::ClientRequest::get(url).finish() {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::OK => {
                        future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                    }
                    status => future::Either::B(future::err(Error::CannotListSessionBlobs(status))),
                })
                .and_then(|answer_json: Vec<BlobInfo>| future::ok(answer_json.into_iter())),
        )
    }
    /// gets information about hub session
    pub fn info(&self) -> impl Future<Item = HubSessionSpec, Error = Error> {
        let url = format!(
            "{}sessions/{}",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        match client::ClientRequest::get(url).finish() {
            Ok(r) => future::Either::A(r.send().map_err(Error::CannotSendRequest).and_then(
                |response| match response.status() {
                    http::StatusCode::OK => {
                        future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                    }
                    status => future::Either::B(future::err(Error::CannotGetHubSession(status))),
                },
            )),
            Err(e) => future::Either::B(future::err(Error::CannotCreateRequest(e))),
        }
    }
    /// sets hub session config
    pub fn set_config(&self, config: Metadata) -> impl Future<Item = (), Error = Error> {
        let url = format!(
            "{}sessions/{}/config",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        future::result(client::ClientRequest::put(url).json(config))
            .map_err(Error::CannotCreateRequest)
            .and_then(|request| request.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::OK => future::ok(()),
                status => future::err(Error::CannotSetHubSessionConfig(status)),
            })
    }
    /// gets hub session config
    pub fn config(&self) -> impl Future<Item = Metadata, Error = Error> {
        let url = format!(
            "{}sessions/{}/config",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        future::result(client::ClientRequest::get(url).finish())
            .map_err(Error::CannotCreateRequest)
            .and_then(|request| request.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::OK => {
                    future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                }
                status => future::Either::B(future::err(Error::CannotGetHubSessionConfig(status))),
            })
    }
    /// updates hub session
    pub fn update(&self, command: session::Command) -> impl Future<Item = (), Error = Error> {
        let url = format!(
            "{}sessions/{}",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        future::result(
            client::ClientRequest::build()
                .method(actix_web::http::Method::PATCH)
                .uri(url)
                .json(command),
        )
        .map_err(Error::CannotCreateRequest)
        .and_then(|request| request.send().map_err(Error::CannotSendRequest))
        .and_then(|response| match response.status() {
            http::StatusCode::OK => future::ok(()),
            status => future::err(Error::CannotUpdateHubSession(status)),
        })
    }
    /// deletes hub session
    pub fn delete(self) -> impl Future<Item = (), Error = Error> {
        let url = format!(
            "{}sessions/{}",
            self.hub_connection.hub_connection_inner.url, self.session_id
        );
        self.hub_connection.delete_resource(&url)
    }
}

impl AsyncRelease for HubSession {
    type Result = Box<Future<Item = (), Error = Error>>;
    fn release(self) -> Self::Result {
        Box::new(self.delete())
    }
}

/// Large binary object.
#[derive(Clone, Debug)]
pub struct Blob {
    hub_session: HubSession,
    blob_id: String,
}

impl Blob {
    /// uploads blob represented by a stream
    pub fn upload_from_stream<S, T>(&self, stream: S) -> impl Future<Item = (), Error = Error>
    where
        S: Stream<Item = Bytes, Error = T> + 'static,
        T: Into<actix_web::Error>,
    {
        let url = format!(
            "{}sessions/{}/blobs/{}",
            self.hub_session.hub_connection.hub_connection_inner.url,
            self.hub_session.session_id,
            self.blob_id
        );
        let request = match client::ClientRequest::put(url).streaming(stream) {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::OK => future::ok(()),
                    status => future::err(Error::CannotUploadBlobFromStream(status)),
                }),
        )
    }
    /// downloads blob
    pub fn download(&self) -> impl Stream<Item = Bytes, Error = Error> {
        let url = format!(
            "{}sessions/{}/blobs/{}",
            self.hub_session.hub_connection.hub_connection_inner.url,
            self.hub_session.session_id,
            self.blob_id
        );
        future::result(client::ClientRequest::get(url).finish())
            .map_err(Error::CannotCreateRequest)
            .and_then(|request| request.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::OK => {
                    future::ok(response.payload().map_err(Error::CannotReceiveBlobBody))
                }
                status => future::err(Error::CannotReceiveBlob(status)),
            })
            .flatten_stream()
    }
    /// deletes blob
    pub fn delete(self) -> impl Future<Item = (), Error = Error> {
        let remove_url = format!(
            "{}sessions/{}/blobs/{}",
            self.hub_session.hub_connection.hub_connection_inner.url,
            self.hub_session.session_id,
            self.blob_id
        );
        let request = match client::ClientRequest::delete(remove_url).finish() {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::OK => future::ok(()),
                    status_code => future::err(Error::CannotDeleteBlob(status_code)),
                }),
        )
    }
}

/// Peer node.
#[derive(Clone, Debug)]
pub struct Peer {
    hub_session: HubSession,
    pub node_id: NodeId,
}

impl Peer {
    /// creates new peer session
    pub fn new_session(
        &self,
        session_info: envman::CreateSession,
    ) -> impl Future<Item = PeerSession, Error = Error> {
        let url = format!(
            "{}sessions/{}/peers/{}/deployments",
            self.hub_session.hub_connection.hub_connection_inner.url,
            self.hub_session.session_id,
            self.node_id.to_string()
        );
        let request = match client::ClientRequest::post(url).json(session_info) {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        let peer_copy = self.clone();
        future::Either::B(
            request
                .send()
                .timeout(Duration::from_secs(3600))
                .map_err(Error::CannotSendRequest)
                .and_then(|response| {
                    if response.status() != http::StatusCode::CREATED {
                        return future::Either::A(future::err(Error::CannotCreatePeerSession(
                            response.status(),
                        )));
                    }
                    future::Either::B(response.json().map_err(Error::InvalidJSONResponse))
                })
                .and_then(|answer_json: String| {
                    future::ok(PeerSession {
                        peer: peer_copy,
                        session_id: answer_json,
                    })
                }),
        )
    }
    /// gets peer information
    pub fn info(&self) -> impl Future<Item = PeerInfo, Error = Error> {
        let url = format!(
            "{}peers/{:?}",
            self.hub_session.hub_connection.hub_connection_inner.url, self.node_id
        );
        future::result(client::ClientRequest::get(&url).finish())
            .map_err(Error::CannotCreateRequest)
            .and_then(|request| request.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::OK => {
                    future::Either::A(response.json().map_err(Error::InvalidJSONResponse))
                }
                status => future::Either::B(future::err(Error::CannotGetPeerInfo(status))),
            })
    }
}

/// Peer session.
#[derive(Clone, Debug)]
pub struct PeerSession {
    peer: Peer,
    session_id: String,
}

impl PeerSession {
    /// updates deployment session by sending multiple peer commands
    pub fn update(
        &self,
        commands: Vec<envman::Command>,
    ) -> impl Future<Item = Vec<String>, Error = Error> {
        let url = format!(
            "{}sessions/{}/peers/{}/deployments/{}",
            self.peer
                .hub_session
                .hub_connection
                .hub_connection_inner
                .url,
            self.peer.hub_session.session_id,
            self.peer.node_id.to_string(),
            self.session_id,
        );
        future::result(
            client::ClientRequest::build()
                .method(actix_web::http::Method::PATCH)
                .uri(url)
                .json(commands),
        )
        .map_err(Error::CannotCreateRequest)
        .and_then(|request| request.send().map_err(Error::CannotSendRequest))
        .and_then(|response| match response.status() {
            http::StatusCode::OK => {
                future::Either::A(response.json().map_err(|e| Error::InvalidJSONResponse(e)))
            }
            status => future::Either::B(future::err(Error::CannotUpdateDeployment(status))),
        })
    }
    /// deletes peer session
    pub fn delete(self) -> impl Future<Item = (), Error = Error> {
        let remove_url = format!(
            "{}sessions/{}/peers/{}/deployments/{}",
            self.peer
                .hub_session
                .hub_connection
                .hub_connection_inner
                .url,
            self.peer.hub_session.session_id,
            self.peer.node_id.to_string(),
            self.session_id,
        );
        let request = match client::ClientRequest::delete(remove_url).finish() {
            Ok(r) => r,
            Err(e) => return future::Either::A(future::err(Error::CannotCreateRequest(e))),
        };
        future::Either::B(
            request
                .send()
                .map_err(Error::CannotSendRequest)
                .and_then(|response| match response.status() {
                    http::StatusCode::OK => future::ok(()),
                    status_code => future::err(Error::CannotDeletePeerSession(status_code)),
                }),
        )
    }
}

impl AsyncRelease for PeerSession {
    type Result = Box<Future<Item = (), Error = Error>>;
    fn release(self) -> Self::Result {
        Box::new(self.delete())
    }
}

pub struct ProviderRef {
    connection: HubConnection,
    node_id: NodeId,
}

pub struct DeploymentRef {
    connection: HubConnection,
    node_id: NodeId,
    info: DeploymentInfo,
}

impl DeploymentRef {
    pub fn id(&self) -> &str {
        self.info.id.as_ref()
    }

    pub fn name(&self) -> &str {
        self.info.name.as_ref()
    }

    pub fn tags<'a>(&'a self) -> impl Iterator<Item = impl AsRef<str> + 'a> {
        self.info.tags.iter() //.map(|v| v.as_ref())
    }

    pub fn note(&self) -> Option<&str> {
        self.info.note.as_ref().map(AsRef::as_ref)
    }

    pub fn delete(self) -> impl Future<Item = (), Error = Error> {
        let url = format!(
            "{}peers/{:?}/deployments/{}",
            self.connection.url(),
            &self.node_id,
            &self.info.id
        );
        client::delete(url)
            .finish()
            .into_future()
            .map_err(Error::CannotCreateRequest)
            .and_then(|r| r.send().map_err(Error::CannotSendRequest))
            .and_then(|response| match response.status() {
                http::StatusCode::NO_CONTENT => future::ok(()),
                status_code => future::err(Error::CannotDeletePeerSession(status_code)),
            })
    }
}

impl ProviderRef {
    pub fn info(&self) -> impl Future<Item = PeerInfo, Error = Error> {
        let url = format!("{}peers/{:?}", self.connection.url(), self.node_id);
        self.connection.fetch_json(&url)
    }

    pub fn deployments(
        &self,
    ) -> impl Future<Item = impl IntoIterator<Item = DeploymentRef>, Error = Error> {
        let url = format!(
            "{}peers/{:?}/deployments",
            self.connection.url(),
            self.node_id
        );
        let connection = self.connection.clone();
        let node_id = self.node_id.clone();

        self.connection
            .fetch_json(&url)
            .and_then(move |list: Vec<_>| {
                Ok(list.into_iter().map(move |i| DeploymentRef {
                    connection: connection.clone(),
                    node_id: node_id.clone(),
                    info: i,
                }))
            })
    }

    pub fn deployment<DeploymentId: AsRef<str>>(
        &self,
        deployment_id: DeploymentId,
    ) -> impl Future<Item = DeploymentRef, Error = Error> {
        let url = format!(
            "{}peers/{:?}/deployments/{}",
            self.connection.url(),
            self.node_id,
            deployment_id.as_ref(),
        );
        let connection = self.connection.clone();
        let node_id = self.node_id.clone();
        self.connection
            .fetch_json(&url)
            .and_then(move |info: DeploymentInfo| {
                Ok(DeploymentRef {
                    connection,
                    node_id,
                    info,
                })
            })
    }
}
