use crate::pow::POW;
use anyhow::{anyhow, Result};
use fs_extra;
use log::{error, info, warn};
use std::{env, fs, io};
use std::fs::create_dir;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{self, Duration};
use uuid::Uuid;
pub struct Handler {
    pub port: String,
    pub compose_dir: String,
    pub pow_difficulty: usize,
    pub pow_timeout: u64,
    pub service_timeout: u64,
}
struct Client {
    socket: TcpStream,
    addr: SocketAddr,
    pass_pow: bool,
    service_name: Option<String>,
    temp_dir: Option<String>,
}
#[derive(thiserror::Error, Debug)]
pub enum HandlerError {
    #[error("PoW timeout")]
    PoWTimeout,
    #[error("Client closes the connection")]
    ClientClose,
    #[error("Service timeout")]
    ServiceTimeout,
    #[error("Connection error")]
    ConnectionError,
}
impl Handler {
    pub async fn handle(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        info!("Listening on port {}", self.port);
        loop {
            let (socket, addr) = listener.accept().await?;
            info!("New connection: {}", addr);
            let self_clone = self.clone();
            let client = Arc::new(Mutex::new(Client {
                socket,
                addr,
                pass_pow: false,
                service_name: None,
                temp_dir: None,
            }));

            tokio::spawn(async move {
                if let Err(e) = self_clone.handle_connect(&client).await {
                    let client_clone = Arc::clone(&client);
                    let mut client_lock = client_clone.lock().await;
                    warn!("Connection {}: {}", client_lock.addr, e);
                    let _ = client_lock.socket.write_all(e.to_string().as_bytes()).await;
                    if let Err(disconnect_error) = self_clone.handle_disconnect(&client_lock).await
                    {
                        error!(
                            "Failed to disconnect {}: {}",
                            client_lock.addr, disconnect_error
                        );
                    }
                    if !e.to_string().contains("Broken pipe") && !e.to_string().contains("closes the conne"){
                            if let Err(_) = client_lock.socket.shutdown().await {
                            eprint!("Failed to shutdown");
                        };
                    }
                }
            });
        }
    }
    async fn handle_connect(&self, client: &Arc<Mutex<Client>>) -> Result<()> {
        let mut client_lock = client.lock().await;
        let mut buf = vec![0; 64];
        let pow = POW::init(self.pow_difficulty);
        client_lock
            .socket
            .write_all(
                format!(
                    "Welcome to the proof of work challenge\n\
                    You have {} seconds to solve the PoW\n\
                    assert sha256('{}' + ?).hexdigest().startswith('0' * {}) == True\n\
                    ? = ",
                    self.pow_timeout.to_string(),
                    pow.nonce_str.clone().unwrap(),
                    self.pow_difficulty.to_string()
                )
                .as_bytes(),
            )
            .await?;
        loop {
            match time::timeout(
                Duration::from_secs(self.pow_timeout),
                client_lock.socket.read(&mut buf),
            )
            .await
            {
                Ok(Ok(n)) => loop {
                    let input = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                    if !client_lock.pass_pow {
                        if pow.verify(input) {
                            info!("Pow accepted for {}", client_lock.addr);
                            client_lock.pass_pow = true;
                            client_lock
                                .socket
                                .write_all(
                                    format!(
                                        "\nPoW accepted, starting service, you have {} seconds\n",
                                        self.service_timeout.to_string()
                                    )
                                    .as_bytes(),
                                )
                                .await?;
                            let port = self.start_service(&mut client_lock).await?;
                            client_lock
                                .socket
                                .write_all(format!("Service started on port {}\n", port).as_bytes())
                                .await?;
                            drop(client_lock);
                            self.handle_pass_pow(client).await?;
                            return Err(anyhow!("handle_pass_pow error"));
                        } else {
                            client_lock
                                .socket
                                .write_all("Invalid PoW\n".as_bytes())
                                .await?;
                            break;
                        }
                    }
                },
                Ok(Err(e)) => {
                    // Client closes the connection
                    eprint!("here");
                    
                    return Err(e.into());
                }
                Err(_) => {
                    // PoW timeout
                    return Err(HandlerError::PoWTimeout.into());
                }
            }
        }
    }
    async fn handle_disconnect(&self, client: &Client) -> Result<()> {
        self.remove_service(client).await?;
        if let Some(temp_dir) = &client.temp_dir {
            fs_extra::remove_items(&vec![temp_dir])?;
            warn!("removed directory {}", temp_dir);
        }
        Ok(())
    }
    async fn handle_pass_pow(&self, client: &Arc<Mutex<Client>>) -> Result<()> {
        let client_clone = Arc::clone(&client);
        let service_timeout = self.service_timeout;
        let handle = tokio::spawn(async move {
            match time::timeout(Duration::from_secs(service_timeout), async {
                let mut client_lock = client_clone.lock().await;
                let mut buf = vec![0; 64];
                loop {
                    match client_lock.socket.read(&mut buf).await {
                        Ok(n) => {
                            if n == 0 {
                                println!("ClientClose");
                                break HandlerError::ClientClose;
                            }
                        }
                        Err(_) => {
                            println!("ConnectionError");
                            break HandlerError::ConnectionError;
                        }
                    }
                }
            })
            .await
            {
                Ok(e) => e,
                Err(_) => HandlerError::ServiceTimeout,
            }
        });
        match handle.await {
            Ok(e) => Err(e.into()),
            Err(e) => Err(e.into()),
        }
    }
    fn copy_recursively(source: &Path, target: &Path) -> io::Result<()> {
        if !source.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Source directory does not exist"));
        }
    
        if !target.exists() {
            fs::create_dir_all(&target)?;
        }
    
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let source_path = entry.path();
            let file_name = entry.file_name();
    
            let target_path = target.join(file_name);
    
            if source_path.is_dir() {
                Self::copy_recursively(&source_path, &target_path)?;
            } else {
                fs::copy(&source_path, &target_path)?;
            }
        }
    
        Ok(())
    }
    async fn start_service(&self, client: &mut Client) -> Result<String> {
        // 1. Create a temporary directory
        // /tmp/pow-compose-[uuid]
        let service_name = format!("pow-compose-{}", Uuid::new_v4().to_string());
        client.service_name = Some(service_name.clone());
        let temp_dir_path = Path::new(env::temp_dir().as_path()).join(&service_name);
        create_dir(&temp_dir_path)?;
        // dbg!(&temp_dir_path);
        client.temp_dir = Some(temp_dir_path.clone().to_string_lossy().to_string());
        let pa: PathBuf = self.compose_dir.clone().into();
        let _ = Self::copy_recursively(pa.as_path(), &temp_dir_path);

        // now we have a temporary compose directory /tmp/pow-compose-[uuid]/[compose_dir]
        // 3. find available port
        let port: String;
        match TcpListener::bind("127.0.0.1:0")
            .await
            .ok()
            .and_then(|listener| listener.local_addr().ok().map(|addr| addr.port()))
        {
            Some(p) => port = p.to_string(),
            None => return Err(anyhow!("No available port")),
        };
        // dbg!(&port);
        // 4. replace port in docker-compose.tpl and write to docker-compose.yml
        let compose_file_path = temp_dir_path.join("docker-compose.tpl");
        let mut compose_file = std::fs::read_to_string(&compose_file_path)?;
        compose_file = compose_file.replace("{{port}}", &port);
        std::fs::write(temp_dir_path.join("docker-compose.yml"), compose_file)?;
        // 5. start the service
        Command::new("docker-compose")
            .args(&["-p", &service_name, "up", "-d"])
            .current_dir(&temp_dir_path)
            .output()
            .await?;
        info!("Open service on port {port}");
        Ok(port)
    }
    async fn remove_service(&self, client: &Client) -> Result<()> {
        let service_name = &client.service_name.clone().unwrap();
        let temp_dir_path = &client.temp_dir.clone().unwrap();
        Command::new("docker-compose")
            .args(&["-p", &service_name, "down"])
            .current_dir(temp_dir_path)
            .output()
            .await?;
        // dbg!(output);
        Ok(())
    }
}
#[tokio::test]
async fn test_handle() {
    let handler = Arc::new(Handler {
        port: "1337".to_string(),
        compose_dir: "./example/".to_string(),
        pow_difficulty: 1,
        pow_timeout: 100,
        service_timeout: 100,
    });
    handler.handle().await.unwrap();
}
