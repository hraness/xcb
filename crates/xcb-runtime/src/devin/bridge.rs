use crate::{Error, Result, private};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader as StdReader, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixListener,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use xcb_core::MAX_JSON_BYTES;

pub(crate) struct Request {
    pub value: Value,
    pub bytes: usize,
    pub reply: oneshot::Sender<Option<Value>>,
}

pub(crate) struct DevinBridge {
    path: PathBuf,
    token: String,
    pub receiver: mpsc::Receiver<Request>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

pub(crate) async fn frame<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    crate::wire_helpers::frame(
        reader,
        &mut Vec::new(),
        MAX_JSON_BYTES,
        "Devin bridge frame bound",
        "incomplete Devin bridge frame",
    )
    .await
}

impl DevinBridge {
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub(crate) fn bind(path: &Path) -> Result<Self> {
        let parent = path.parent().ok_or(Error::PrivateState)?;
        private::check_directory(parent)?;
        if !path.is_absolute() || path.as_os_str().len() > 100 || path.symlink_metadata().is_ok() {
            return Err(Error::PrivateState);
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expected = token.clone();
        let (sender, receiver) = mpsc::channel(16);
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let serving = async move {
                let (stream, _) = listener.accept().await?;
                drop(listener);
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                let hello = frame(&mut read)
                    .await?
                    .ok_or(Error::Protocol("Devin bridge authentication absent"))?;
                let hello: Value = serde_json::from_slice(&hello)?;
                if hello.as_object().map(|o| o.len()) != Some(1)
                    || hello["token"].as_str() != Some(expected.as_str())
                {
                    return Err(Error::Protocol("Devin bridge authentication failed"));
                }
                let mut total = 0usize;
                for _ in 0..4096 {
                    let Some(bytes) = frame(&mut read).await? else {
                        return Ok::<_, Error>(());
                    };
                    total = total
                        .checked_add(bytes.len())
                        .filter(|n| *n <= 64 * 1024 * 1024)
                        .ok_or(Error::Protocol("Devin bridge output bound"))?;
                    let value = serde_json::from_slice(&bytes)?;
                    let (reply, response) = oneshot::channel();
                    sender
                        .send(Request {
                            value,
                            bytes: bytes.len(),
                            reply,
                        })
                        .await
                        .map_err(|_| Error::Protocol("Devin bridge receiver closed"))?;
                    if let Some(response) = response
                        .await
                        .map_err(|_| Error::Protocol("Devin bridge reply absent"))?
                    {
                        crate::wire_helpers::write_frame(
                            &mut write,
                            &response,
                            MAX_JSON_BYTES,
                            "Devin bridge reply bound",
                        )
                        .await?;
                    }
                }
                Err(Error::Protocol("Devin bridge request bound"))
            };
            tokio::select! { _=stopped.changed()=>{}, _=serving=>{} }
        });
        Ok(Self {
            path: path.to_owned(),
            token,
            receiver,
            stop,
            task: Some(task),
        })
    }

    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub(crate) fn configuration(&self, helper: &Path) -> Result<Value> {
        let helper = helper
            .to_str()
            .filter(|s| helper.is_absolute() && !s.chars().any(char::is_control))
            .ok_or(Error::PrivateState)?;
        let path = self.path.to_str().ok_or(Error::PrivateState)?;
        Ok(
            json!({"mcpServers":{"xcb":{"command":helper,"args":["broker-stdio"],"env":{"XCB_BROKER_SOCKET":path,"XCB_BROKER_TOKEN":self.token}}}}),
        )
    }

    pub(crate) async fn shutdown(&mut self) -> bool {
        let _ = self.stop.send(true);
        let Some(mut task) = self.task.take() else {
            return true;
        };
        match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
            Ok(_) => true,
            Err(_) => {
                task.abort();
                let _ = task.await;
                true
            }
        }
    }
}

impl Drop for DevinBridge {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn copy_lines(mut from: impl BufRead, mut to: impl Write) -> Result<()> {
    for _ in 0..4096 {
        let Some(bytes) = crate::wire_helpers::frame_sync(
            &mut from,
            MAX_JSON_BYTES,
            "MCP stdio frame bound",
            "incomplete MCP stdio frame",
        )?
        else {
            return Ok(());
        };
        let _: Value = serde_json::from_slice(&bytes)?;
        to.write_all(&bytes)?;
        to.flush()?;
    }
    Err(Error::Protocol("MCP stdio request bound"))
}

/// Hidden child-process entry point; this relay never executes a tool. Its
/// process is covered by the provider process-group join before lease release.
pub async fn broker_stdio(path: &Path, token: &str) -> Result<()> {
    if !path.is_absolute() || !xcb_core::hex64_any(token) {
        return Err(Error::Protocol("invalid broker connection"));
    }
    let path = path.to_owned();
    let token = token.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut socket = std::os::unix::net::UnixStream::connect(path)?;
        crate::wire_helpers::write_frame_sync(
            &mut socket,
            &json!({"token":token}),
            MAX_JSON_BYTES,
            "broker hello bound",
        )?;
        let input_socket = socket.try_clone()?;
        let input = std::thread::spawn(move || {
            let result = copy_lines(std::io::stdin().lock(), &input_socket);
            let _ = input_socket.shutdown(std::net::Shutdown::Write);
            result
        });
        let result = copy_lines(StdReader::new(&socket), std::io::stdout().lock());
        let _ = socket.shutdown(std::net::Shutdown::Both);
        // A provider with an open stdin is stopped by its owning process group.
        // Do not hold the relay's return hostage to that detached input reader.
        drop(input);
        result
    })
    .await
    .map_err(|_| Error::Protocol("broker relay task failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::AsyncWriteExt, net::UnixStream};

    #[tokio::test]
    async fn authenticated_bridge_preserves_the_exact_request_and_reply() {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().canonicalize().unwrap().join("broker.sock");
        let mut bridge = DevinBridge::bind(&path).unwrap();
        let config = bridge.configuration(Path::new("/synthetic/xcb")).unwrap();
        assert_eq!(config["mcpServers"]["xcb"]["args"], json!(["broker-stdio"]));
        assert_eq!(
            config["mcpServers"]["xcb"]["env"]["XCB_BROKER_TOKEN"],
            bridge.token
        );
        assert!(bridge.configuration(Path::new("relative-helper")).is_err());
        let mut client = UnixStream::connect(path).await.unwrap();
        let query = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}});
        let bytes = format!("{}\n{}\n", json!({"token":bridge.token}), query);
        client.write_all(bytes.as_bytes()).await.unwrap();
        let request = tokio::time::timeout(Duration::from_secs(3), bridge.receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.value, query);
        let expected = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
        request.reply.send(Some(expected.clone())).unwrap();
        let got = frame(&mut BufReader::new(client)).await.unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&got).unwrap(), expected);
        assert!(bridge.shutdown().await);
    }

    #[tokio::test]
    async fn wrong_token_never_reaches_the_broker() {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().canonicalize().unwrap().join("broker.sock");
        let mut bridge = DevinBridge::bind(&path).unwrap();
        let mut client = UnixStream::connect(path).await.unwrap();
        client.write_all(b"{\"token\":\"wrong\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), bridge.receiver.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(bridge.shutdown().await);
    }

    #[tokio::test]
    async fn shutdown_joins_a_partial_frame_without_waiting_on_peer_input() {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().canonicalize().unwrap().join("broker.sock");
        let mut bridge = DevinBridge::bind(&path).unwrap();
        let mut client = UnixStream::connect(path).await.unwrap();
        client.write_all(b"{\"token\":").await.unwrap();
        assert!(bridge.shutdown().await);
        assert!(bridge.task.is_none());
        assert!(bridge.receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn bridge_frame_rejects_overflow_before_newline() {
        let bytes = vec![b'x'; MAX_JSON_BYTES + 1];
        let mut reader = &bytes[..];
        assert!(frame(&mut reader).await.is_err());
    }
}
