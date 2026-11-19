//! Connects a TCP socket through `wasi:sockets`; exists only to be refused.

#[no_mangle]
pub extern "C" fn probe() -> usize {
    std::net::TcpStream::connect("93.184.216.34:443").map(|_| 1).unwrap_or(0)
}
