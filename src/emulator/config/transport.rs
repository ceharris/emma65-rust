use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ExpandedPathBuf;
use crate::emulator::{PipeTransport, PtyTransport, TcpSocketTransport, Transport, TransportError, TransportRelay, TransportReporter, UnixSocketTransport};

/// A transport configuration spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportSpec {
    Tcp { port: u16, address: IpAddr },
    Unix { path: ExpandedPathBuf },
    Pty { path: Option<ExpandedPathBuf> },
    /// Spawn a child process and bridge the device's byte stream to its stdin/stdout.
    ///
    /// `command[0]` is the executable path; `command[1..]` are arguments.
    /// The child's stderr is inherited from the emulator process.
    /// Any exit of the child process triggers the error reporter supplied to
    /// [`TransportSpec::to_transport_with_reporter`].
    ///
    /// CLI shorthand: `pipe:/path/to/exe,arg1,arg2`
    /// TOML: `transport = { pipe = { command = ["/path/to/exe", "arg1"] } }`
    Pipe { command: Vec<String> },
}

impl TransportSpec {
    const DEFAULT_BIND_IP_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

    /// Creates the transport and its paired relay, reporting connect/disconnect/error
    /// events on `reporter` and calling `on_child_exit` if the child process spawned by
    /// a [`Pipe`](TransportSpec::Pipe) variant exits for any reason. For all other
    /// variants the callback is never invoked.
    pub async fn to_transport_with_reporter<F>(
        &self,
        reporter: TransportReporter,
        on_child_exit: F,
    ) -> Result<(Box<dyn Transport>, TransportRelay), TransportError>
    where
        F: FnOnce(std::io::Error) + Send + 'static,
    {
        self.to_transport_with_reporter_and_capacity(reporter, on_child_exit, None).await
    }

    /// Same as [`to_transport_with_reporter`](Self::to_transport_with_reporter), but lets
    /// the caller override the outbound/inbound ring capacity for a
    /// [`Pipe`](TransportSpec::Pipe) variant — e.g. a device module that knows its own
    /// bulk per-frame payload size at instantiate time can size the ring to fit it exactly,
    /// rather than the crate's default `CHANNEL_CAPACITY`. `capacity: None` behaves exactly
    /// like [`to_transport_with_reporter`](Self::to_transport_with_reporter); ignored for
    /// every variant other than `Pipe`.
    pub async fn to_transport_with_reporter_and_capacity<F>(
        &self,
        reporter: TransportReporter,
        on_child_exit: F,
        capacity: Option<usize>,
    ) -> Result<(Box<dyn Transport>, TransportRelay), TransportError>
    where
        F: FnOnce(std::io::Error) + Send + 'static,
    {
        match self {
            TransportSpec::Pipe { command } => {
                let (transport, relay) = match capacity {
                    Some(capacity) => PipeTransport::spawn_with_capacity(command, reporter, on_child_exit, capacity).await,
                    None => PipeTransport::spawn(command, reporter, on_child_exit).await,
                }.map_err(TransportError::Io)?;
                Ok((Box::new(transport), TransportRelay::Byte(relay)))
            }
            other => other.to_transport(reporter).await,
        }
    }

    /// Creates the transport and its paired relay, reporting connect/disconnect/error
    /// events on `reporter`. For [`Pipe`](TransportSpec::Pipe) variants, child process
    /// exit is silently ignored; use
    /// [`to_transport_with_reporter`](Self::to_transport_with_reporter) to surface exit
    /// events as emulator errors.
    async fn to_transport(&self, reporter: TransportReporter) -> Result<(Box<dyn Transport>, TransportRelay), TransportError> {
        match self {
            TransportSpec::Tcp { port, address} => {
                let addr = SocketAddr::new(*address, *port);
                let (transport, relay) = TcpSocketTransport::listen(addr, reporter).await?;
                Ok((Box::new(transport), TransportRelay::Tagged(relay)))
            }
            TransportSpec::Unix { path } => {
                let (transport, relay) = UnixSocketTransport::listen(path, reporter).await?;
                Ok((Box::new(transport), TransportRelay::Tagged(relay)))
            }
            TransportSpec::Pty { path} => {
                let (transport, relay) = if let Some(path) = path {
                    PtyTransport::open(Some(path), reporter)
                } else {
                    PtyTransport::open(None, reporter)
                }?;
                Ok((Box::new(transport), TransportRelay::Byte(relay)))
            }
            TransportSpec::Pipe { command } => {
                let (transport, relay) = PipeTransport::spawn(command, reporter, |_| {}).await
                    .map_err(TransportError::Io)?;
                Ok((Box::new(transport), TransportRelay::Byte(relay)))
            }
        }
    }

}

impl FromStr for TransportSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        match parts[0] {
            "tcp" if parts.len() == 2 => if let Ok(port) = parts[1].parse::<u16>() {
                Ok(TransportSpec::Tcp { port, address: TransportSpec::DEFAULT_BIND_IP_ADDR })
            } else if let Ok(socket_addr) = parts[1].parse::<SocketAddr>() {
                Ok(TransportSpec::Tcp { port: socket_addr.port(), address: socket_addr.ip() })
            } else {
                Err(format!("Invalid IP address or port '{}' in transport spec", parts[1]))
            },
            "tcp" => Err("TCP transport spec format is 'tcp:PORT' or 'tcp:IP-ADDR:PORT'".to_string()),
            "unix" if parts.len() == 2 => {
                if parts[1].trim() != "" {
                    Ok(TransportSpec::Unix { path: ExpandedPathBuf::new(parts[1]) })
                } else {
                    Err("Path name is required".to_string())
                }
            },
            "unix" => Err("Unix-domain transport spec format is 'unix:PATHNAME'".to_string()),
            "pty" if parts.len() == 2 => {
                if parts[1].trim() != "" {
                    Ok(TransportSpec::Pty { path: Some(ExpandedPathBuf::new(parts[1])) })
                } else {
                    Err("Path name is required".to_string())
                }
            },
            "pty" if parts.len() == 1 => Ok(TransportSpec::Pty { path: None }),
            "pty" => Err("PTY transport spec format is 'pty[:SYMLINK-NAME]'".to_string()),
            "pipe" if parts.len() == 2 => {
                let args_str = parts[1].trim();
                if args_str.is_empty() {
                    return Err("Pipe transport spec format is 'pipe:EXECUTABLE[,ARG,...]'".to_string());
                }
                let command: Vec<String> = args_str.split(',').map(str::to_owned).collect();
                Ok(TransportSpec::Pipe { command })
            }
            "pipe" => Err("Pipe transport spec format is 'pipe:EXECUTABLE[,ARG,...]'".to_string()),
            "" => Err("Transport spec expected".to_string()),
            _ => Err(format!("Invalid transport type or arguments: '{}'", s))
        }
    }
}

/// A configuration format for a [`TransportSpec`].
#[derive(Deserialize)]
#[serde(untagged)]
pub enum TransportSpecFormat {
    /// Plain string from a CLI argument.
    Shorthand(String),
    /// Structured table from a TOML configuration.
    Structured(TransportSpec),
}

impl TryFrom<TransportSpecFormat> for TransportSpec {
    type Error = String;

    fn try_from(wire: TransportSpecFormat) -> Result<Self, Self::Error> {
        match wire {
            TransportSpecFormat::Shorthand(s) => TransportSpec::from_str(&s),
            TransportSpecFormat::Structured(t) => Ok(t),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{DeviceEvent, device_event_channel};
    use std::net::Ipv6Addr;
    use std::path::PathBuf;
    use tokio::net::UnixStream;

    fn tmp_socket_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/emma65_test_transport_spec_{}.sock", name))
    }

    /// Regression coverage for the bug this issue fixes: `to_transport_with_reporter`
    /// used to always construct its own `TransportReporter::pending(None)` internally,
    /// discarding whatever reporter the caller passed in, so connect/disconnect/error
    /// events could never reach a real `ErrorSender` no matter what the caller supplied.
    #[tokio::test]
    async fn to_transport_with_reporter_forwards_caller_reporter_for_pipe_spec() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-42");

        let spec = TransportSpec::Pipe { command: vec!["cat".to_string()] };
        let (mut transport, relay) = spec.to_transport_with_reporter(reporter, |_| {}).await.unwrap();

        match receiver.try_recv() {
            Ok(DeviceEvent::TransportConnected { device, .. }) => assert_eq!(device, "test-device-42"),
            other => panic!("expected TransportConnected, got {other:?}"),
        }

        transport.shutdown();
        drop((transport, relay));
    }

    /// Same regression, for a delegated (non-`Pipe`) variant — exercises the
    /// `to_transport_with_reporter` -> `to_transport` delegation path.
    #[tokio::test]
    async fn to_transport_with_reporter_forwards_caller_reporter_for_unix_spec() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-7");

        let path = tmp_socket_path("forwards_caller_reporter");
        let spec = TransportSpec::Unix { path: ExpandedPathBuf::new(path.to_str().unwrap()) };
        let (mut transport, relay) = spec.to_transport_with_reporter(reporter, |_| {}).await.unwrap();

        let _client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        match receiver.try_recv() {
            Ok(DeviceEvent::TransportConnected { device, .. }) => assert_eq!(device, "test-device-7"),
            other => panic!("expected TransportConnected, got {other:?}"),
        }

        transport.shutdown();
        drop((transport, relay));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[should_panic(expected = "Transport spec expected")]
    fn from_str_with_empty_string() {
        TransportSpec::from_str("").unwrap();
    }

    #[test]
    #[should_panic(expected = "Invalid transport type")]
    fn from_str_with_unrecognized_transport_type() {
        TransportSpec::from_str("foobar").unwrap();
    }

    #[test]
    #[should_panic(expected = "Invalid transport type")]
    fn from_str_with_unrecognized_transport_type_and_args() {
        TransportSpec::from_str("foobar:fizzbuzz").unwrap();
    }

    #[test]
    #[should_panic(expected = "format")]
    fn from_str_with_tcp_and_no_arguments() {
        TransportSpec::from_str("tcp").unwrap();
    }

    #[test]
    #[should_panic(expected = "Invalid IP")]
    fn from_str_with_tcp_and_missing_arguments() {
        TransportSpec::from_str("tcp:").unwrap();
    }

    #[test]
    fn from_str_with_tcp_port_only() {
        let spec = TransportSpec::from_str("tcp:10001").unwrap();
        match spec {
            TransportSpec::Tcp { port, address } => {
                assert_eq!(port, 10001);
                assert_eq!(address, TransportSpec::DEFAULT_BIND_IP_ADDR);
            },
            _ => panic!("expected TCP transport")
        }
    }

    #[test]
    fn from_str_with_tcp_ipv4_address_and_port() {
        let spec = TransportSpec::from_str("tcp:192.168.1.1:10001").unwrap();
        match spec {
            TransportSpec::Tcp { port, address } => {
                assert_eq!(port, 10001);
                assert_eq!(address, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
            },
            _ => panic!("expected TCP transport")
        }
    }

    #[test]
    fn from_str_with_tcp_ipv6_address_and_port() {
        let spec = TransportSpec::from_str("tcp:[::1]:10001").unwrap();
        match spec {
            TransportSpec::Tcp { port, address }=> {
                assert_eq!(port, 10001);
                assert_eq!(address, IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)));
            },
            _ => panic!("expected TCP transport")
        }
    }

    #[test]
    #[should_panic(expected = "Invalid IP")]
    fn from_str_with_tcp_and_ip_address_without_port() {
        TransportSpec::from_str("tcp:127.0.0.1").unwrap();
    }

    #[test]
    #[should_panic(expected = "Invalid IP")]
    fn from_str_with_tcp_and_invalid_ip_address() {
        TransportSpec::from_str("tcp:256.256.256.256:10000").unwrap();
    }

    #[test]
    #[should_panic(expected = "Invalid IP")]
    fn from_str_with_tcp_and_invalid_port() {
        TransportSpec::from_str("tcp:1.1.1.1:65536").unwrap();
    }

    #[test]
    fn from_str_with_unix_and_pathname() {
        let spec = TransportSpec::from_str("unix:tmp/my.socket").unwrap();
        match spec {
            TransportSpec::Unix { path }  => {
                assert_eq!(&*path, std::path::Path::new("tmp/my.socket"));
            }
            _ => panic!("expected Unix transport")
        }
    }

    #[test]
    #[should_panic(expected = "format")]
    fn from_str_with_unix_and_no_arguments() {
        TransportSpec::from_str("unix").unwrap();
    }

    #[test]
    #[should_panic(expected = "required")]
    fn from_str_with_unix_and_empty_path() {
        TransportSpec::from_str("unix:").unwrap();
    }

    #[test]
    fn from_str_with_pty_and_pathname() {
        let spec = TransportSpec::from_str("pty:tmp/my.pty").unwrap();
        match spec {
            TransportSpec::Pty { path: Some(path) }  => {
                assert_eq!(&*path, std::path::Path::new("tmp/my.pty"));
            }
            _ => panic!("expected PTY transport  with pathname")
        }
    }

    #[test]
    fn from_str_with_pty_no_pathname() {
        let spec = TransportSpec::from_str("pty").unwrap();
        match spec {
            TransportSpec::Pty { path: None }  => {}
            _ => panic!("expected PTY transport")
        }
    }


    #[test]
    #[should_panic(expected = "required")]
    fn from_str_with_pty_no_empty() {
        TransportSpec::from_str("pty:").unwrap();
    }

    #[test]
    fn from_str_with_pipe_and_executable_only() {
        let spec = TransportSpec::from_str("pipe:/usr/bin/cat").unwrap();
        match spec {
            TransportSpec::Pipe { command } => {
                assert_eq!(command, vec!["/usr/bin/cat".to_string()]);
            }
            _ => panic!("expected Pipe transport")
        }
    }

    #[test]
    fn from_str_with_pipe_and_arguments() {
        let spec = TransportSpec::from_str("pipe:/usr/bin/foo,--bar,baz").unwrap();
        match spec {
            TransportSpec::Pipe { command } => {
                assert_eq!(command, vec![
                    "/usr/bin/foo".to_string(),
                    "--bar".to_string(),
                    "baz".to_string(),
                ]);
            }
            _ => panic!("expected Pipe transport")
        }
    }

    #[test]
    #[should_panic(expected = "format")]
    fn from_str_with_pipe_and_no_arguments() {
        TransportSpec::from_str("pipe").unwrap();
    }

    #[test]
    #[should_panic(expected = "format")]
    fn from_str_with_pipe_and_empty_command() {
        TransportSpec::from_str("pipe:").unwrap();
    }

}
