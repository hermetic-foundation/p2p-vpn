use p2p_vpn::wire::{HEADER_LEN, WIRE_VERSION};

fn main() {
    println!("p2p-vpn: packet wire v{WIRE_VERSION}, fixed header {HEADER_LEN} bytes");
}
