use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::emulator::{Transport, TransportError};

/// A transport configuration spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportSpec {
    Tcp { port: u16, address: IpAddr },
    Unix { path: std::path::PathBuf },
    Pty { path: Option<std::path::PathBuf> },
}

impl TransportSpec {
    const DEFAULT_BIND_IP_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    
    fn to_transport(&self) -> Result<Box<dyn Transport>, TransportError> {
        todo!()
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
                    Ok(TransportSpec::Unix { path: parts[1].into() })
                } else {
                    Err("Path name is required".to_string())
                }
            },
            "unix" => Err("Unix-domain transport spec format is 'unix:PATHNAME'".to_string()),
            "pty" if parts.len() == 2 => {
                if parts[1].trim() != "" {
                    Ok(TransportSpec::Pty { path: Some(parts[1].into()) })
                } else {
                    Err("Path name is required".to_string())
                }
            },
            "pty" if parts.len() == 1 => Ok(TransportSpec::Pty { path: None }),
            "pty" => Err("PTY transport spec format is 'pty[:SYMLINK-NAME]'".to_string()),
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
    use std::net::Ipv6Addr;

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
                assert_eq!(path, std::path::PathBuf::from("tmp/my.socket"));
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
                assert_eq!(path, std::path::PathBuf::from("tmp/my.pty"));
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

}
