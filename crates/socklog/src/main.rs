use libc;
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixDatagram;

fn main() -> io::Result<()> {
    // Get socket name from command line args
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <abstract_socket_name>", args[0]);
        std::process::exit(1);
    }
    let socket_name = &args[1];

    // Create a raw socket
    let sock_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0) };
    if sock_fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // Prepare the sockaddr_un structure
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

    // Abstract namespace starts with a null byte
    // First byte is \0, followed by the name
    let name_bytes = socket_name.as_bytes();
    let mut abstract_name = Vec::with_capacity(name_bytes.len() + 1);
    abstract_name.push(0);
    abstract_name.extend_from_slice(name_bytes);

    // Copy the name to sun_path, converting u8 to i8
    let path_len = std::cmp::min(abstract_name.len(), addr.sun_path.len());
    for i in 0..path_len {
        addr.sun_path[i] = abstract_name[i] as i8;
    }

    // Bind the socket
    let addr_len = std::mem::size_of::<libc::sa_family_t>() + path_len;
    let bind_result = unsafe {
        libc::bind(
            sock_fd,
            &addr as *const _ as *const libc::sockaddr,
            addr_len as libc::socklen_t,
        )
    };
    if bind_result < 0 {
        unsafe { libc::close(sock_fd) };
        return Err(io::Error::last_os_error());
    }

    // Convert raw fd to UnixDatagram
    let socket = unsafe { UnixDatagram::from_raw_fd(sock_fd) };
    println!("Listening on abstract socket: {}", socket_name);

    // Buffer for receiving messages
    let mut buf = vec![0; 65536]; // Maximum UDP datagram size

    loop {
        match socket.recv(&mut buf) {
            Ok(size) => {
                let data = &buf[..size];

                // Try to convert to UTF-8 string
                match std::str::from_utf8(data) {
                    Ok(string) => {
                        println!("[{}]", string);
                    }
                    Err(_) => {
                        // If not valid UTF-8, print as hex
                        for byte in data {
                            print!("{:02x}", byte);
                        }
                        println!();
                    }
                }

                // Ensure output is flushed
                io::stdout().flush()?;
            }
            Err(e) => {
                eprintln!("Error receiving datagram: {}", e);
            }
        }
    }
}
