extern crate actix;
#[macro_use]
extern crate serde_derive;
extern crate actix_web;
extern crate gu_net;
extern crate serde;
#[cfg(test)]
extern crate serde_json;

use actix::prelude::*;
use gu_net::rpc::peer::PeerSessionInfo;
use std::path::PathBuf;
use std::{fmt, io};

/// Errors
// impl note: can not use error_chain bc it does not support SerDe
#[derive(Serialize, Deserialize, Debug)]
pub enum Error {
    Error(String),
    IoError(String),
    NoSuchSession(String),
    NoSuchChild(String),
    UnknownEnv(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::IoError(e.to_string())
    }
}

impl From<actix::MailboxError> for Error {
    fn from(e: MailboxError) -> Self {
        Error::Error(format!("{}", e))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Error(msg) => write!(f, "error: {}", msg)?,
            Error::IoError(msg) => write!(f, "IO error: {}", msg)?,
            Error::NoSuchSession(msg) => write!(f, "session not found: {}", msg)?,
            Error::NoSuchChild(msg) => write!(f, "child not found: {}", msg)?,
            Error::UnknownEnv(env_id) => write!(f, "unknown exec environment: {}", env_id)?,
        }
        Ok(())
    }
}

impl From<String> for Error {
    fn from(msg: String) -> Self {
        Error::Error(msg)
    }
}

/// image with binaries and resources for given session
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub url: String,
    pub hash: String,
}

/// Message for session creation: local provisioning: downloads and unpacks the binaries
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSession {
    pub env_type: String,
    pub image: Image,
    pub name: String,
    pub tags: Vec<String>,
    pub note: Option<String>,
}

impl CreateSession {
    pub const ID: u32 = 37;
}

/// returns session_id
impl Message for CreateSession {
    type Result = Result<String, Error>;
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub session_id: String,
    pub commands: Vec<Command>,
}

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Command {
    Exec {
        // return cmd output
        executable: String,
        args: Vec<String>,
    },
    Start {
        // return child process id
        executable: String,
        args: Vec<String>,
        // TODO: consider adding tags here
    },
    #[serde(rename_all = "camelCase")]
    Stop {
        child_id: String,
    },
    AddTags(Vec<String>),
    DelTags(Vec<String>),
    DownloadFile {
        uri: String,
        file_path: PathBuf,
    },
    UploadFile {
        uri: String,
        file_path: PathBuf,
    },
}

impl SessionUpdate {
    pub const ID: u32 = 38;
}

impl Message for SessionUpdate {
    type Result = Result<Vec<String>, Vec<String>>;
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetSessions {}

impl GetSessions {
    pub const ID: u32 = 39;
}

impl Message for GetSessions {
    type Result = Result<Vec<PeerSessionInfo>, ()>;
}

/// Message for session destruction: clean local resources and kill all child processes
#[derive(Serialize, Deserialize)]
pub struct DestroySession {
    pub session_id: String,
}

impl DestroySession {
    pub const ID: u32 = 40;
}

impl Message for DestroySession {
    type Result = Result<String, Error>;
}

#[cfg(test)]
mod test {

    use super::*;
    use serde_json;

    #[test]
    fn test_update_1() {
        let json = r#"
        {
            "sessionId":"hd::08087f8f-a0f3-41d4-a192-3388f46aa678",
            "commands":[
                {"exec":{"executable":"gu-mine","args":["spec"]}}
            ]
        }
        "#;

        let u: SessionUpdate = serde_json::from_str(json).unwrap();

        assert_eq!(u.session_id, "hd::08087f8f-a0f3-41d4-a192-3388f46aa678");

        let json = r#"
        {
            "sessionId":"hd::4c562af4-db3f-4e57-8fac-cf30249db682",
            "commands":[{"stop":{"childId":"145ccba6-ce24-4809-8856-7eae40092fdd"}},{"delTags":["gu:mine:working"]}]
        }"#;

        let u: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(u.session_id, "hd::4c562af4-db3f-4e57-8fac-cf30249db682");
    }

}