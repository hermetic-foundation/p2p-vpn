use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

const IPV4_MIN_HEADER_LEN: usize = 20;
const IPV6_HEADER_LEN: usize = 40;
const MAX_IPV6_EXTENSION_HEADERS: usize = 16;

const IP_PROTOCOL_ICMP: u8 = 1;
const IP_PROTOCOL_TCP: u8 = 6;
const IP_PROTOCOL_UDP: u8 = 17;
const IP_PROTOCOL_IPV6_FRAGMENT: u8 = 44;
const IP_PROTOCOL_ICMPV6: u8 = 58;
const IP_PROTOCOL_NO_NEXT_HEADER: u8 = 59;
const IP_PROTOCOL_MOBILITY: u8 = 135;
const IP_PROTOCOL_UDP_LITE: u8 = 136;
const IP_PROTOCOL_HIP: u8 = 139;
const IP_PROTOCOL_SHIM6: u8 = 140;

const IPV4_OPTION_END: u8 = 0;
const IPV4_OPTION_NOP: u8 = 1;
const IPV4_OPTION_LOOSE_SOURCE_ROUTE: u8 = 131;
const IPV4_OPTION_STRICT_SOURCE_ROUTE: u8 = 137;
const IPV6_OPTION_HOME_ADDRESS: u8 = 201;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PrimaryAddresses {
    pub ipv4: Ipv4Addr,
    pub ipv6: Ipv6Addr,
}

impl PrimaryAddresses {
    pub(crate) fn contains(self, address: IpAddr) -> bool {
        match address {
            IpAddr::V4(address) => address == self.ipv4,
            IpAddr::V6(address) => address == self.ipv6,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PacketTranslator {
    presentation: PrimaryAddresses,
    overlay: PrimaryAddresses,
}

impl PacketTranslator {
    pub(crate) const fn new(presentation: PrimaryAddresses, overlay: PrimaryAddresses) -> Self {
        Self {
            presentation,
            overlay,
        }
    }

    pub(crate) fn translate_outbound(
        self,
        packet: &mut [u8],
    ) -> Result<Translation, TranslationError> {
        translate_outer(packet, Direction::Outbound, self.presentation, self.overlay)
    }

    pub(crate) fn translate_inbound(
        self,
        packet: &mut [u8],
    ) -> Result<Translation, TranslationError> {
        translate_outer(packet, Direction::Inbound, self.overlay, self.presentation)
    }

    pub(crate) fn outbound_requires_translation(self, source: IpAddr) -> bool {
        match source {
            IpAddr::V4(source) => {
                source == self.presentation.ipv4 && self.presentation.ipv4 != self.overlay.ipv4
            }
            IpAddr::V6(source) => {
                source == self.presentation.ipv6 && self.presentation.ipv6 != self.overlay.ipv6
            }
        }
    }

    pub(crate) fn validate_supported(packet: &[u8]) -> Result<(), TranslationError> {
        validate_supported_packet(packet)
    }
}

pub(crate) fn validate_packet_isolation(packet: &[u8]) -> Result<(), TranslationError> {
    let Some(version) = packet.first().map(|byte| byte >> 4) else {
        return Err(TranslationError::TooShort);
    };
    match version {
        4 => {
            let header_len = validate_ipv4_packet(packet, true)?;
            validate_ipv4_options(packet, header_len)
        }
        6 => {
            validate_ipv6_packet(packet, true)?;
            reject_forbidden_address_semantics(ipv6_transport(packet, true)?)
        }
        version => Err(TranslationError::UnsupportedIpVersion(version)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Translation {
    Unchanged,
    Translated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranslationError {
    TooShort,
    InvalidLength,
    InvalidIpv4Header,
    InvalidIpv6ExtensionHeader,
    TooManyIpv6ExtensionHeaders,
    UnsupportedIpVersion(u8),
    UnsupportedTransport(u8),
    UnsupportedAddressSemantics(u8),
    InvalidTransportChecksum(u8),
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(formatter, "translated packet is truncated"),
            Self::InvalidLength => write!(formatter, "translated packet length is invalid"),
            Self::InvalidIpv4Header => write!(formatter, "translated IPv4 header is invalid"),
            Self::InvalidIpv6ExtensionHeader => {
                write!(formatter, "translated IPv6 extension header is invalid")
            }
            Self::TooManyIpv6ExtensionHeaders => {
                write!(formatter, "translated IPv6 extension chain is too long")
            }
            Self::UnsupportedIpVersion(version) => {
                write!(formatter, "translated IP version {version} is unsupported")
            }
            Self::UnsupportedTransport(protocol) => {
                write!(
                    formatter,
                    "translated IP protocol {protocol} is unsupported"
                )
            }
            Self::UnsupportedAddressSemantics(value) => {
                write!(
                    formatter,
                    "translated IP address option {value} is unsupported"
                )
            }
            Self::InvalidTransportChecksum(protocol) => {
                write!(
                    formatter,
                    "translated IP protocol {protocol} has an invalid checksum"
                )
            }
        }
    }
}

impl std::error::Error for TranslationError {}

#[derive(Clone, Copy)]
enum Direction {
    Outbound,
    Inbound,
}

#[derive(Clone, Copy)]
struct Transport {
    protocol: u8,
    offset: usize,
    fragmented: bool,
    non_initial_fragment: bool,
    forbidden_address_semantics: Option<u8>,
}

fn translate_outer(
    packet: &mut [u8],
    direction: Direction,
    from: PrimaryAddresses,
    to: PrimaryAddresses,
) -> Result<Translation, TranslationError> {
    let Some(version) = packet.first().map(|byte| byte >> 4) else {
        return Err(TranslationError::TooShort);
    };
    match version {
        4 => translate_outer_ipv4(packet, direction, from.ipv4, to.ipv4),
        6 => translate_outer_ipv6(packet, direction, from.ipv6, to.ipv6),
        version => Err(TranslationError::UnsupportedIpVersion(version)),
    }
}

fn translate_outer_ipv4(
    packet: &mut [u8],
    direction: Direction,
    from: Ipv4Addr,
    to: Ipv4Addr,
) -> Result<Translation, TranslationError> {
    let (header_len, transport) = validate_supported_ipv4(packet)?;
    if from == to {
        return Ok(Translation::Unchanged);
    }
    let field = match direction {
        Direction::Outbound => 12,
        Direction::Inbound => 16,
    };
    if packet[field..field + 4] != from.octets() {
        return Ok(Translation::Unchanged);
    }
    packet[field..field + 4].copy_from_slice(&to.octets());
    update_transport_after_address_change(
        packet,
        transport,
        &from.octets(),
        &to.octets(),
        IpFamily::V4,
    )?;
    if transport.protocol == IP_PROTOCOL_ICMP && !transport.non_initial_fragment {
        translate_icmpv4_quote(packet, transport.offset, from, to, transport.fragmented)?;
    }
    write_ipv4_header_checksum(packet, header_len);
    Ok(Translation::Translated)
}

fn translate_outer_ipv6(
    packet: &mut [u8],
    direction: Direction,
    from: Ipv6Addr,
    to: Ipv6Addr,
) -> Result<Translation, TranslationError> {
    let transport = validate_supported_ipv6(packet)?;
    if from == to {
        return Ok(Translation::Unchanged);
    }
    let field = match direction {
        Direction::Outbound => 8,
        Direction::Inbound => 24,
    };
    if packet[field..field + 16] != from.octets() {
        return Ok(Translation::Unchanged);
    }
    packet[field..field + 16].copy_from_slice(&to.octets());
    update_transport_after_address_change(
        packet,
        transport,
        &from.octets(),
        &to.octets(),
        IpFamily::V6,
    )?;
    if transport.protocol == IP_PROTOCOL_ICMPV6 && !transport.non_initial_fragment {
        translate_icmpv6_quote(packet, transport.offset, from, to, transport.fragmented)?;
        if !transport.fragmented {
            write_icmpv6_checksum(packet, transport.offset)?;
        }
    }
    Ok(Translation::Translated)
}

#[derive(Clone, Copy)]
enum IpFamily {
    V4,
    V6,
}

fn validate_supported_packet(packet: &[u8]) -> Result<(), TranslationError> {
    let Some(version) = packet.first().map(|byte| byte >> 4) else {
        return Err(TranslationError::TooShort);
    };
    match version {
        4 => validate_supported_ipv4(packet).map(|_| ()),
        6 => validate_supported_ipv6(packet).map(|_| ()),
        version => Err(TranslationError::UnsupportedIpVersion(version)),
    }
}

fn validate_supported_ipv4(packet: &[u8]) -> Result<(usize, Transport), TranslationError> {
    let header_len = validate_ipv4_packet(packet, true)?;
    validate_ipv4_options(packet, header_len)?;
    let transport = ipv4_transport(packet, header_len);
    validate_transport(packet, transport, IpFamily::V4)?;
    Ok((header_len, transport))
}

fn validate_supported_ipv6(packet: &[u8]) -> Result<Transport, TranslationError> {
    validate_ipv6_packet(packet, true)?;
    let transport = ipv6_transport(packet, true)?;
    reject_forbidden_address_semantics(transport)?;
    validate_transport(packet, transport, IpFamily::V6)?;
    Ok(transport)
}

fn validate_transport(
    packet: &[u8],
    transport: Transport,
    family: IpFamily,
) -> Result<(), TranslationError> {
    // Headers after an IPv6 Fragment header belong to the fragmentable part. A
    // non-initial fragment therefore cannot be parsed as an independent chain.
    if matches!(family, IpFamily::V6) && transport.non_initial_fragment {
        return Ok(());
    }
    if matches!(
        transport.protocol,
        IP_PROTOCOL_MOBILITY | IP_PROTOCOL_HIP | IP_PROTOCOL_SHIM6
    ) {
        return Err(TranslationError::UnsupportedTransport(transport.protocol));
    }
    let supported = matches!(
        (family, transport.protocol),
        (
            IpFamily::V4 | IpFamily::V6,
            IP_PROTOCOL_TCP | IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE
        ) | (IpFamily::V4, IP_PROTOCOL_ICMP)
            | (IpFamily::V6, IP_PROTOCOL_ICMPV6)
            | (_, IP_PROTOCOL_NO_NEXT_HEADER)
    );
    if !supported {
        return Err(TranslationError::UnsupportedTransport(transport.protocol));
    }
    if transport.non_initial_fragment || transport.protocol == IP_PROTOCOL_NO_NEXT_HEADER {
        return Ok(());
    }
    let checksum_offset = match transport.protocol {
        IP_PROTOCOL_TCP => transport.offset + 16,
        IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE => transport.offset + 6,
        IP_PROTOCOL_ICMP | IP_PROTOCOL_ICMPV6 => transport.offset + 2,
        _ => return Ok(()),
    };
    if checksum_offset + 2 > packet.len() {
        return Err(TranslationError::TooShort);
    }
    if !matches!(transport.protocol, IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE) {
        return Ok(());
    }
    let checksum = u16::from_be_bytes([packet[checksum_offset], packet[checksum_offset + 1]]);
    if checksum == 0
        && !matches!(
            (family, transport.protocol),
            (IpFamily::V4, IP_PROTOCOL_UDP)
        )
    {
        return Err(TranslationError::InvalidTransportChecksum(
            transport.protocol,
        ));
    }
    Ok(())
}

fn update_transport_after_address_change(
    packet: &mut [u8],
    transport: Transport,
    old_address: &[u8],
    new_address: &[u8],
    family: IpFamily,
) -> Result<(), TranslationError> {
    if transport.non_initial_fragment {
        return Ok(());
    }
    let checksum_offset = match (family, transport.protocol) {
        (IpFamily::V4 | IpFamily::V6, IP_PROTOCOL_TCP) => transport.offset + 16,
        (IpFamily::V4 | IpFamily::V6, IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE) => {
            transport.offset + 6
        }
        (IpFamily::V6, IP_PROTOCOL_ICMPV6) if transport.fragmented => transport.offset + 2,
        (IpFamily::V6, IP_PROTOCOL_ICMPV6) => return Ok(()),
        (IpFamily::V4, IP_PROTOCOL_ICMP) => return Ok(()),
        (_, IP_PROTOCOL_NO_NEXT_HEADER) => return Ok(()),
        (_, protocol) => return Err(TranslationError::UnsupportedTransport(protocol)),
    };
    if checksum_offset + 2 > packet.len() {
        return Err(TranslationError::TooShort);
    }
    let checksum = u16::from_be_bytes([packet[checksum_offset], packet[checksum_offset + 1]]);
    if checksum == 0 {
        if matches!(
            (family, transport.protocol),
            (IpFamily::V4, IP_PROTOCOL_UDP)
        ) {
            return Ok(());
        }
        if matches!(transport.protocol, IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE) {
            return Err(TranslationError::InvalidTransportChecksum(
                transport.protocol,
            ));
        }
    }
    let updated = adjust_checksum(checksum, old_address, new_address);
    let updated = canonical_datagram_checksum(transport.protocol, updated);
    packet[checksum_offset..checksum_offset + 2].copy_from_slice(&updated.to_be_bytes());
    Ok(())
}

fn adjust_checksum(checksum: u16, old: &[u8], new: &[u8]) -> u16 {
    debug_assert_eq!(old.len(), new.len());
    let mut sum = u32::from(!checksum);
    let mut old_words = old.chunks_exact(2);
    let mut new_words = new.chunks_exact(2);
    for (old_word, new_word) in old_words.by_ref().zip(new_words.by_ref()) {
        sum = sum
            .wrapping_add(u32::from(!u16::from_be_bytes([old_word[0], old_word[1]])))
            .wrapping_add(u32::from(u16::from_be_bytes([new_word[0], new_word[1]])));
        sum = fold_checksum(sum);
    }
    if let (Some(old_byte), Some(new_byte)) =
        (old_words.remainder().first(), new_words.remainder().first())
    {
        sum = sum
            .wrapping_add(u32::from(!(u16::from(*old_byte) << 8)))
            .wrapping_add(u32::from(*new_byte) << 8);
    }
    !u16::try_from(fold_checksum(sum)).expect("folded checksum fits in u16")
}

fn fold_checksum(mut sum: u32) -> u32 {
    while sum > u32::from(u16::MAX) {
        sum = (sum & u32::from(u16::MAX)) + (sum >> 16);
    }
    sum
}

fn canonical_datagram_checksum(protocol: u8, checksum: u16) -> u16 {
    if matches!(protocol, IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE) && checksum == 0 {
        u16::MAX
    } else {
        checksum
    }
}

fn validate_ipv4_packet(packet: &[u8], exact_length: bool) -> Result<usize, TranslationError> {
    if packet.len() < IPV4_MIN_HEADER_LEN {
        return Err(TranslationError::TooShort);
    }
    let header_len = usize::from(packet[0] & 0x0f) * 4;
    if header_len < IPV4_MIN_HEADER_LEN || packet.len() < header_len {
        return Err(TranslationError::InvalidIpv4Header);
    }
    let declared_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if declared_len < header_len
        || (exact_length && declared_len != packet.len())
        || (!exact_length && declared_len < packet.len().min(header_len))
    {
        return Err(TranslationError::InvalidLength);
    }
    Ok(header_len)
}

fn validate_ipv6_packet(packet: &[u8], exact_length: bool) -> Result<(), TranslationError> {
    if packet.len() < IPV6_HEADER_LEN {
        return Err(TranslationError::TooShort);
    }
    let declared_len = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    if exact_length && declared_len.saturating_add(IPV6_HEADER_LEN) != packet.len() {
        return Err(TranslationError::InvalidLength);
    }
    Ok(())
}

fn validate_ipv4_options(packet: &[u8], header_len: usize) -> Result<(), TranslationError> {
    let mut offset = IPV4_MIN_HEADER_LEN;
    while offset < header_len {
        let option = packet[offset];
        match option {
            IPV4_OPTION_END => return Ok(()),
            IPV4_OPTION_NOP => offset += 1,
            IPV4_OPTION_LOOSE_SOURCE_ROUTE | IPV4_OPTION_STRICT_SOURCE_ROUTE => {
                return Err(TranslationError::UnsupportedAddressSemantics(option));
            }
            _ => {
                if offset + 2 > header_len {
                    return Err(TranslationError::InvalidIpv4Header);
                }
                let length = usize::from(packet[offset + 1]);
                if length < 2 || offset + length > header_len {
                    return Err(TranslationError::InvalidIpv4Header);
                }
                offset += length;
            }
        }
    }
    Ok(())
}

fn ipv4_transport(packet: &[u8], header_len: usize) -> Transport {
    let fragment = u16::from_be_bytes([packet[6], packet[7]]);
    Transport {
        protocol: packet[9],
        offset: header_len,
        fragmented: fragment & 0x3fff != 0,
        non_initial_fragment: fragment & 0x1fff != 0,
        forbidden_address_semantics: None,
    }
}

fn ipv6_transport(packet: &[u8], strict: bool) -> Result<Transport, TranslationError> {
    let mut protocol = packet[6];
    let mut offset = IPV6_HEADER_LEN;
    let mut fragmented = false;
    let mut non_initial_fragment = false;
    let mut forbidden_address_semantics = None;
    for _ in 0..MAX_IPV6_EXTENSION_HEADERS {
        match protocol {
            0 | 43 | 60 => {
                if offset + 2 > packet.len() {
                    return if strict {
                        Err(TranslationError::InvalidIpv6ExtensionHeader)
                    } else {
                        Ok(Transport {
                            protocol,
                            offset,
                            fragmented,
                            non_initial_fragment,
                            forbidden_address_semantics,
                        })
                    };
                }
                let next = packet[offset];
                let length = (usize::from(packet[offset + 1]) + 1) * 8;
                if length < 8 || offset + length > packet.len() {
                    return if strict {
                        Err(TranslationError::InvalidIpv6ExtensionHeader)
                    } else {
                        Ok(Transport {
                            protocol,
                            offset,
                            fragmented,
                            non_initial_fragment,
                            forbidden_address_semantics,
                        })
                    };
                }
                if protocol == 43 {
                    forbidden_address_semantics.get_or_insert(43);
                } else if ipv6_options_contain_home_address(packet, offset, length)? {
                    forbidden_address_semantics.get_or_insert(IPV6_OPTION_HOME_ADDRESS);
                }
                protocol = next;
                offset += length;
            }
            IP_PROTOCOL_IPV6_FRAGMENT => {
                if offset + 8 > packet.len() {
                    return if strict {
                        Err(TranslationError::InvalidIpv6ExtensionHeader)
                    } else {
                        Ok(Transport {
                            protocol,
                            offset,
                            fragmented,
                            non_initial_fragment,
                            forbidden_address_semantics,
                        })
                    };
                }
                let next = packet[offset];
                let fragment = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
                fragmented = fragment & 0xfff9 != 0;
                non_initial_fragment = fragment & 0xfff8 != 0;
                protocol = next;
                offset += 8;
                if non_initial_fragment {
                    return Ok(Transport {
                        protocol,
                        offset,
                        fragmented,
                        non_initial_fragment,
                        forbidden_address_semantics,
                    });
                }
            }
            _ => {
                return Ok(Transport {
                    protocol,
                    offset,
                    fragmented,
                    non_initial_fragment,
                    forbidden_address_semantics,
                });
            }
        }
    }
    Err(TranslationError::TooManyIpv6ExtensionHeaders)
}

fn ipv6_options_contain_home_address(
    packet: &[u8],
    header_offset: usize,
    header_len: usize,
) -> Result<bool, TranslationError> {
    let end = header_offset + header_len;
    let mut offset = header_offset + 2;
    while offset < end {
        let option = packet[offset];
        if option == 0 {
            offset += 1;
            continue;
        }
        if offset + 2 > end {
            return Err(TranslationError::InvalidIpv6ExtensionHeader);
        }
        let option_len = usize::from(packet[offset + 1]);
        if offset + 2 + option_len > end {
            return Err(TranslationError::InvalidIpv6ExtensionHeader);
        }
        if option == IPV6_OPTION_HOME_ADDRESS {
            return Ok(true);
        }
        offset += 2 + option_len;
    }
    Ok(false)
}

fn reject_forbidden_address_semantics(transport: Transport) -> Result<(), TranslationError> {
    if let Some(value) = transport.forbidden_address_semantics {
        return Err(TranslationError::UnsupportedAddressSemantics(value));
    }
    Ok(())
}

fn translate_icmpv4_quote(
    packet: &mut [u8],
    offset: usize,
    from: Ipv4Addr,
    to: Ipv4Addr,
    fragmented: bool,
) -> Result<(), TranslationError> {
    if offset + 4 > packet.len() {
        return Err(TranslationError::TooShort);
    }
    let error_type = matches!(packet[offset], 3 | 4 | 5 | 11 | 12);
    if error_type && offset + 8 < packet.len() {
        let checksum = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
        let original_quote = fragmented.then(|| packet[offset + 8..].to_vec());
        {
            let quoted = &mut packet[offset + 8..];
            if quoted.first().is_some_and(|byte| byte >> 4 == 4) {
                translate_quoted_ipv4(quoted, from, to)?;
            }
        }
        if let Some(original_quote) = original_quote {
            let updated = adjust_checksum(checksum, &original_quote, &packet[offset + 8..]);
            packet[offset + 2..offset + 4].copy_from_slice(&updated.to_be_bytes());
        } else {
            packet[offset + 2] = 0;
            packet[offset + 3] = 0;
            let checksum = internet_checksum(&packet[offset..]);
            packet[offset + 2..offset + 4].copy_from_slice(&checksum.to_be_bytes());
        }
    }
    Ok(())
}

fn translate_icmpv6_quote(
    packet: &mut [u8],
    offset: usize,
    from: Ipv6Addr,
    to: Ipv6Addr,
    fragmented: bool,
) -> Result<(), TranslationError> {
    if offset + 4 > packet.len() {
        return Err(TranslationError::TooShort);
    }
    if packet[offset] < 128 && offset + 8 < packet.len() {
        let checksum = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
        let original_quote = fragmented.then(|| packet[offset + 8..].to_vec());
        {
            let quoted = &mut packet[offset + 8..];
            if quoted.first().is_some_and(|byte| byte >> 4 == 6) {
                translate_quoted_ipv6(quoted, from, to)?;
            }
        }
        if let Some(original_quote) = original_quote {
            let updated = adjust_checksum(checksum, &original_quote, &packet[offset + 8..]);
            packet[offset + 2..offset + 4].copy_from_slice(&updated.to_be_bytes());
        }
    }
    Ok(())
}

fn translate_quoted_ipv4(
    packet: &mut [u8],
    from: Ipv4Addr,
    to: Ipv4Addr,
) -> Result<(), TranslationError> {
    let header_len = validate_ipv4_packet(packet, false)?;
    let mut changes = 0;
    for field in [12, 16] {
        if packet[field..field + 4] == from.octets() {
            packet[field..field + 4].copy_from_slice(&to.octets());
            changes += 1;
        }
    }
    if changes > 0 {
        let transport = ipv4_transport(packet, header_len);
        for _ in 0..changes {
            adjust_quoted_transport_checksum(
                packet,
                transport,
                &from.octets(),
                &to.octets(),
                IpFamily::V4,
            );
        }
        write_ipv4_header_checksum(packet, header_len);
    }
    Ok(())
}

fn translate_quoted_ipv6(
    packet: &mut [u8],
    from: Ipv6Addr,
    to: Ipv6Addr,
) -> Result<(), TranslationError> {
    validate_ipv6_packet(packet, false)?;
    let mut changes = 0;
    for field in [8, 24] {
        if packet[field..field + 16] == from.octets() {
            packet[field..field + 16].copy_from_slice(&to.octets());
            changes += 1;
        }
    }
    if changes > 0 {
        let transport = ipv6_transport(packet, false)?;
        if transport.forbidden_address_semantics.is_none() {
            for _ in 0..changes {
                adjust_quoted_transport_checksum(
                    packet,
                    transport,
                    &from.octets(),
                    &to.octets(),
                    IpFamily::V6,
                );
            }
        }
    }
    Ok(())
}

fn adjust_quoted_transport_checksum(
    packet: &mut [u8],
    transport: Transport,
    old_address: &[u8],
    new_address: &[u8],
    family: IpFamily,
) {
    if transport.non_initial_fragment {
        return;
    }
    let checksum_offset = match (family, transport.protocol) {
        (IpFamily::V4 | IpFamily::V6, IP_PROTOCOL_TCP) => transport.offset + 16,
        (IpFamily::V4 | IpFamily::V6, IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE) => {
            transport.offset + 6
        }
        _ => return,
    };
    if checksum_offset + 2 > packet.len() {
        return;
    }
    let checksum = u16::from_be_bytes([packet[checksum_offset], packet[checksum_offset + 1]]);
    if checksum == 0 && matches!(transport.protocol, IP_PROTOCOL_UDP | IP_PROTOCOL_UDP_LITE) {
        return;
    }
    let updated = adjust_checksum(checksum, old_address, new_address);
    let updated = canonical_datagram_checksum(transport.protocol, updated);
    packet[checksum_offset..checksum_offset + 2].copy_from_slice(&updated.to_be_bytes());
}

fn write_ipv4_header_checksum(packet: &mut [u8], header_len: usize) {
    packet[10] = 0;
    packet[11] = 0;
    let checksum = internet_checksum(&packet[..header_len]);
    packet[10..12].copy_from_slice(&checksum.to_be_bytes());
}

fn write_icmpv6_checksum(packet: &mut [u8], offset: usize) -> Result<(), TranslationError> {
    if offset + 4 > packet.len() {
        return Err(TranslationError::TooShort);
    }
    packet[offset + 2] = 0;
    packet[offset + 3] = 0;
    let upper_len =
        u32::try_from(packet.len() - offset).map_err(|_| TranslationError::InvalidLength)?;
    let mut sum = 0_u32;
    sum = checksum_bytes(sum, &packet[8..24]);
    sum = checksum_bytes(sum, &packet[24..40]);
    sum = checksum_bytes(sum, &upper_len.to_be_bytes());
    sum = checksum_bytes(sum, &[0, 0, 0, IP_PROTOCOL_ICMPV6]);
    sum = checksum_bytes(sum, &packet[offset..]);
    let checksum = !u16::try_from(fold_checksum(sum)).expect("folded checksum fits in u16");
    packet[offset + 2..offset + 4].copy_from_slice(&checksum.to_be_bytes());
    Ok(())
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    !u16::try_from(fold_checksum(checksum_bytes(0, bytes))).expect("folded checksum fits in u16")
}

fn checksum_bytes(mut sum: u32, bytes: &[u8]) -> u32 {
    let mut words = bytes.chunks_exact(2);
    for word in &mut words {
        sum = sum.wrapping_add(u32::from(u16::from_be_bytes([word[0], word[1]])));
        sum = fold_checksum(sum);
    }
    if let Some(byte) = words.remainder().first() {
        sum = sum.wrapping_add(u32::from(*byte) << 8);
    }
    fold_checksum(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESENTATION_V4: Ipv4Addr = Ipv4Addr::new(100, 64, 1, 10);
    const OVERLAY_V4: Ipv4Addr = Ipv4Addr::new(100, 64, 2, 20);
    const REMOTE_V4: Ipv4Addr = Ipv4Addr::new(100, 64, 3, 30);
    const PRESENTATION_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 1, 2, 3, 4, 5, 6, 7);
    const OVERLAY_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 2, 2, 3, 4, 5, 6, 8);
    const REMOTE_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 3, 2, 3, 4, 5, 6, 9);

    fn translator() -> PacketTranslator {
        PacketTranslator::new(
            PrimaryAddresses {
                ipv4: PRESENTATION_V4,
                ipv6: PRESENTATION_V6,
            },
            PrimaryAddresses {
                ipv4: OVERLAY_V4,
                ipv6: OVERLAY_V6,
            },
        )
    }

    #[test]
    fn ipv4_tcp_translation_round_trips_with_valid_checksums() {
        let original = ipv4_tcp_packet(PRESENTATION_V4, REMOTE_V4);
        let mut packet = original.clone();

        assert_eq!(
            translator().translate_outbound(&mut packet),
            Ok(Translation::Translated)
        );
        assert_eq!(&packet[12..16], &OVERLAY_V4.octets());
        assert_ipv4_checksum(&packet);
        assert_tcp_v4_checksum(&packet);

        let response = ipv4_tcp_packet(REMOTE_V4, OVERLAY_V4);
        let mut response_packet = response;
        assert_eq!(
            translator().translate_inbound(&mut response_packet),
            Ok(Translation::Translated)
        );
        assert_eq!(&response_packet[16..20], &PRESENTATION_V4.octets());
        assert_ipv4_checksum(&response_packet);
        assert_tcp_v4_checksum(&response_packet);

        assert_eq!(
            translator().translate_outbound(&mut packet),
            Ok(Translation::Unchanged)
        );
    }

    #[test]
    fn ipv4_udp_zero_checksum_remains_disabled() {
        let mut packet = ipv4_udp_packet(PRESENTATION_V4, REMOTE_V4, true);

        translator()
            .translate_outbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[26..28], &[0, 0]);
        assert_ipv4_checksum(&packet);
    }

    #[test]
    fn udp_lite_computed_zero_is_transmitted_as_all_ones() {
        let old_checksum = (u16::MIN..=u16::MAX)
            .find(|checksum| {
                adjust_checksum(*checksum, &PRESENTATION_V4.octets(), &OVERLAY_V4.octets()) == 0
            })
            .expect("address delta has a zero checksum input");
        let mut packet = ipv4_packet(PRESENTATION_V4, REMOTE_V4, IP_PROTOCOL_UDP_LITE, &[0; 8]);
        packet[26..28].copy_from_slice(&old_checksum.to_be_bytes());

        translator()
            .translate_outbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[26..28], &u16::MAX.to_be_bytes());
        assert_ipv4_checksum(&packet);
    }

    #[test]
    fn incremental_checksum_handles_odd_length_changes() {
        let old = [0x12, 0x34, 0x56];
        let new = [0xab, 0xcd, 0xef];

        assert_eq!(
            adjust_checksum(internet_checksum(&old), &old, &new),
            internet_checksum(&new)
        );
    }

    #[test]
    fn invalid_mandatory_zero_checksums_fail_closed() {
        let mut ipv4_udp_lite =
            ipv4_packet(PRESENTATION_V4, REMOTE_V4, IP_PROTOCOL_UDP_LITE, &[0; 8]);
        assert_eq!(
            translator().translate_outbound(&mut ipv4_udp_lite),
            Err(TranslationError::InvalidTransportChecksum(
                IP_PROTOCOL_UDP_LITE
            ))
        );

        let mut ipv6_udp = ipv6_udp_packet(PRESENTATION_V6, REMOTE_V6, false);
        ipv6_udp[46..48].fill(0);
        assert_eq!(
            translator().translate_outbound(&mut ipv6_udp),
            Err(TranslationError::InvalidTransportChecksum(IP_PROTOCOL_UDP))
        );
    }

    #[test]
    fn alternate_ipv6_address_semantics_and_checksums_fail_closed() {
        let ipv4_source_route = ipv4_source_route_packet(
            PRESENTATION_V4,
            REMOTE_V4,
            OVERLAY_V4,
            IPV4_OPTION_LOOSE_SOURCE_ROUTE,
        );
        assert_eq!(
            validate_packet_isolation(&ipv4_source_route),
            Err(TranslationError::UnsupportedAddressSemantics(
                IPV4_OPTION_LOOSE_SOURCE_ROUTE
            ))
        );

        for protocol in [IP_PROTOCOL_MOBILITY, IP_PROTOCOL_HIP, IP_PROTOCOL_SHIM6] {
            let mut packet = ipv6_packet(PRESENTATION_V6, REMOTE_V6, protocol, &[]);
            assert_eq!(
                translator().translate_outbound(&mut packet),
                Err(TranslationError::UnsupportedTransport(protocol))
            );
        }

        let mut routing = ipv6_udp_packet(PRESENTATION_V6, REMOTE_V6, true);
        routing[6] = 43;
        assert_eq!(
            translator().translate_outbound(&mut routing),
            Err(TranslationError::UnsupportedAddressSemantics(43))
        );

        let mut home_address = ipv6_udp_packet(PRESENTATION_V6, REMOTE_V6, true);
        home_address[6] = 60;
        home_address[42] = IPV6_OPTION_HOME_ADDRESS;
        home_address[43] = 4;
        assert_eq!(
            translator().translate_outbound(&mut home_address),
            Err(TranslationError::UnsupportedAddressSemantics(
                IPV6_OPTION_HOME_ADDRESS
            ))
        );
    }

    #[test]
    fn ipv6_udp_translation_handles_extension_headers() {
        let mut packet = ipv6_udp_packet(PRESENTATION_V6, REMOTE_V6, true);

        translator()
            .translate_outbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[8..24], &OVERLAY_V6.octets());
        assert_udp_v6_checksum(&packet, 48);
    }

    #[test]
    fn non_initial_fragments_translate_without_transport_header() {
        let mut packet = ipv6_non_initial_fragment(PRESENTATION_V6, REMOTE_V6);

        translator()
            .translate_outbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[8..24], &OVERLAY_V6.octets());
    }

    #[test]
    fn fragmented_ipv6_extension_chain_reassembles_with_valid_udp_checksum() {
        let mut original = ipv6_udp_packet(PRESENTATION_V6, REMOTE_V6, true);
        original[6] = 60;
        let (mut first, mut second) = fragment_ipv6_packet(&original, 16);

        translator()
            .translate_outbound(&mut first)
            .expect("first fragment translation");
        translator()
            .translate_outbound(&mut second)
            .expect("non-initial fragment translation");

        assert_eq!(&first[8..24], &OVERLAY_V6.octets());
        assert_eq!(&second[8..24], &OVERLAY_V6.octets());
        let mut payload = first[48..].to_vec();
        payload.extend_from_slice(&second[48..]);
        let reassembled = ipv6_packet(OVERLAY_V6, REMOTE_V6, 60, &payload);
        assert_udp_v6_checksum(&reassembled, 48);
    }

    #[test]
    fn fragmented_icmp_errors_reassemble_with_presentation_identity() {
        let quoted_v4 = ipv4_udp_packet_with_payload(REMOTE_V4, OVERLAY_V4, 20);
        let ipv4 = ipv4_icmp_error(REMOTE_V4, OVERLAY_V4, &quoted_v4);
        let (mut ipv4_first, mut ipv4_second) = fragment_ipv4_packet(&ipv4, 40);

        translator()
            .translate_inbound(&mut ipv4_first)
            .expect("first IPv4 fragment translation");
        translator()
            .translate_inbound(&mut ipv4_second)
            .expect("second IPv4 fragment translation");

        assert_eq!(&ipv4_first[16..20], &PRESENTATION_V4.octets());
        assert_eq!(&ipv4_second[16..20], &PRESENTATION_V4.octets());
        let mut reassembled_v4 = ipv4_first[20..].to_vec();
        reassembled_v4.extend_from_slice(&ipv4_second[20..]);
        assert_eq!(internet_checksum(&reassembled_v4), 0);
        assert_eq!(&reassembled_v4[8 + 16..8 + 20], &PRESENTATION_V4.octets());
        assert_udp_v4_checksum(&reassembled_v4[8..]);

        let quoted_v6 = ipv6_udp_packet(REMOTE_V6, OVERLAY_V6, false);
        let ipv6 = ipv6_icmp_error(REMOTE_V6, OVERLAY_V6, &quoted_v6);
        let (mut ipv6_first, mut ipv6_second) = fragment_ipv6_packet(&ipv6, 56);

        translator()
            .translate_inbound(&mut ipv6_first)
            .expect("first IPv6 fragment translation");
        translator()
            .translate_inbound(&mut ipv6_second)
            .expect("second IPv6 fragment translation");

        assert_eq!(&ipv6_first[24..40], &PRESENTATION_V6.octets());
        assert_eq!(&ipv6_second[24..40], &PRESENTATION_V6.octets());
        let mut reassembled_v6 = ipv6_first[48..].to_vec();
        reassembled_v6.extend_from_slice(&ipv6_second[48..]);
        assert_eq!(&reassembled_v6[8 + 24..8 + 40], &PRESENTATION_V6.octets());
        assert_icmpv6_message_checksum(REMOTE_V6, PRESENTATION_V6, &reassembled_v6);
    }

    #[test]
    fn ipv6_atomic_fragment_translates_complete_icmp_quote() {
        let quoted = ipv6_udp_packet(REMOTE_V6, OVERLAY_V6, false);
        let mut packet = ipv6_fragmented_icmp_error(REMOTE_V6, OVERLAY_V6, &quoted, 0);

        translator()
            .translate_inbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[24..40], &PRESENTATION_V6.octets());
        assert_eq!(&packet[48 + 8 + 24..48 + 8 + 40], &PRESENTATION_V6.octets());
        assert_icmpv6_checksum(&packet, 48);
    }

    #[test]
    fn icmpv4_error_translation_updates_quoted_flow() {
        let quoted = ipv4_udp_packet(REMOTE_V4, OVERLAY_V4, false);
        let mut packet = ipv4_icmp_error(REMOTE_V4, OVERLAY_V4, &quoted);

        translator()
            .translate_inbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[16..20], &PRESENTATION_V4.octets());
        assert_eq!(&packet[20 + 8 + 16..20 + 8 + 20], &PRESENTATION_V4.octets());
        assert_ipv4_checksum(&packet);
        assert_eq!(internet_checksum(&packet[20..]), 0);
    }

    #[test]
    fn icmpv6_error_translation_updates_quoted_flow_and_checksum() {
        let quoted = ipv6_udp_packet(REMOTE_V6, OVERLAY_V6, false);
        let mut packet = ipv6_icmp_error(REMOTE_V6, OVERLAY_V6, &quoted);

        translator()
            .translate_inbound(&mut packet)
            .expect("translation");

        assert_eq!(&packet[24..40], &PRESENTATION_V6.octets());
        assert_eq!(&packet[40 + 8 + 24..40 + 8 + 40], &PRESENTATION_V6.octets());
        assert_icmpv6_checksum(&packet, 40);
    }

    #[test]
    fn malformed_and_unknown_translated_packets_fail_closed() {
        let mut short = vec![0x45, 0, 0];
        assert_eq!(
            translator().translate_outbound(&mut short),
            Err(TranslationError::TooShort)
        );

        let mut unknown = ipv4_packet(PRESENTATION_V4, REMOTE_V4, 99, &[0; 8]);
        assert_eq!(
            translator().translate_outbound(&mut unknown),
            Err(TranslationError::UnsupportedTransport(99))
        );
    }

    fn ipv4_tcp_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut tcp = vec![0_u8; 20];
        tcp[0..2].copy_from_slice(&1234_u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&443_u16.to_be_bytes());
        tcp[12] = 5 << 4;
        tcp[13] = 0x10;
        let mut packet = ipv4_packet(source, destination, IP_PROTOCOL_TCP, &tcp);
        write_tcp_udp_v4_checksum(&mut packet, 20, 16);
        packet
    }

    fn ipv4_udp_packet(
        source: Ipv4Addr,
        destination: Ipv4Addr,
        disabled_checksum: bool,
    ) -> Vec<u8> {
        let mut packet = ipv4_udp_packet_with_payload(source, destination, 4);
        if disabled_checksum {
            packet[26..28].fill(0);
        }
        packet
    }

    fn ipv4_udp_packet_with_payload(
        source: Ipv4Addr,
        destination: Ipv4Addr,
        payload_len: usize,
    ) -> Vec<u8> {
        let udp_len = 8 + payload_len;
        let mut udp = vec![0_u8; udp_len];
        udp[0..2].copy_from_slice(&1234_u16.to_be_bytes());
        udp[2..4].copy_from_slice(&53_u16.to_be_bytes());
        udp[4..6].copy_from_slice(&u16::try_from(udp_len).expect("UDP length").to_be_bytes());
        let mut packet = ipv4_packet(source, destination, IP_PROTOCOL_UDP, &udp);
        write_tcp_udp_v4_checksum(&mut packet, 20, 6);
        packet
    }

    fn ipv4_packet(
        source: Ipv4Addr,
        destination: Ipv4Addr,
        protocol: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let total_len = IPV4_MIN_HEADER_LEN + payload.len();
        let mut packet = vec![0_u8; total_len];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(
            &u16::try_from(total_len)
                .expect("test packet length")
                .to_be_bytes(),
        );
        packet[8] = 64;
        packet[9] = protocol;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet[20..].copy_from_slice(payload);
        write_ipv4_header_checksum(&mut packet, IPV4_MIN_HEADER_LEN);
        packet
    }

    fn ipv4_source_route_packet(
        source: Ipv4Addr,
        destination: Ipv4Addr,
        ultimate: Ipv4Addr,
        option: u8,
    ) -> Vec<u8> {
        let mut packet = vec![0_u8; 28];
        packet[0] = 0x47;
        packet[2..4].copy_from_slice(&28_u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = IP_PROTOCOL_NO_NEXT_HEADER;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet[20] = option;
        packet[21] = 7;
        packet[22] = 4;
        packet[23..27].copy_from_slice(&ultimate.octets());
        packet[27] = IPV4_OPTION_END;
        write_ipv4_header_checksum(&mut packet, 28);
        packet
    }

    fn ipv6_udp_packet(source: Ipv6Addr, destination: Ipv6Addr, extension: bool) -> Vec<u8> {
        let extension_len = if extension { 8 } else { 0 };
        let payload_len = extension_len + 12;
        let mut packet = vec![0_u8; IPV6_HEADER_LEN + payload_len];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(
            &u16::try_from(payload_len)
                .expect("payload length")
                .to_be_bytes(),
        );
        packet[6] = if extension { 0 } else { IP_PROTOCOL_UDP };
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        let udp_offset = IPV6_HEADER_LEN + extension_len;
        if extension {
            packet[40] = IP_PROTOCOL_UDP;
            packet[41] = 0;
        }
        packet[udp_offset..udp_offset + 2].copy_from_slice(&1234_u16.to_be_bytes());
        packet[udp_offset + 2..udp_offset + 4].copy_from_slice(&53_u16.to_be_bytes());
        packet[udp_offset + 4..udp_offset + 6].copy_from_slice(&12_u16.to_be_bytes());
        write_udp_v6_checksum(&mut packet, udp_offset);
        packet
    }

    fn ipv6_packet(
        source: Ipv6Addr,
        destination: Ipv6Addr,
        next_header: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = vec![0_u8; IPV6_HEADER_LEN + payload.len()];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(
            &u16::try_from(payload.len())
                .expect("IPv6 payload length")
                .to_be_bytes(),
        );
        packet[6] = next_header;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet[40..].copy_from_slice(payload);
        packet
    }

    fn ipv6_non_initial_fragment(source: Ipv6Addr, destination: Ipv6Addr) -> Vec<u8> {
        let mut packet = vec![0_u8; 56];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(&16_u16.to_be_bytes());
        packet[6] = IP_PROTOCOL_IPV6_FRAGMENT;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet[40] = IP_PROTOCOL_TCP;
        packet[42..44].copy_from_slice(&8_u16.to_be_bytes());
        packet
    }

    fn ipv4_icmp_error(source: Ipv4Addr, destination: Ipv4Addr, quoted: &[u8]) -> Vec<u8> {
        let mut icmp = vec![0_u8; 8 + quoted.len()];
        icmp[0] = 3;
        icmp[1] = 1;
        icmp[8..].copy_from_slice(quoted);
        let checksum = internet_checksum(&icmp);
        icmp[2..4].copy_from_slice(&checksum.to_be_bytes());
        ipv4_packet(source, destination, IP_PROTOCOL_ICMP, &icmp)
    }

    fn fragment_ipv4_packet(packet: &[u8], first_payload_len: usize) -> (Vec<u8>, Vec<u8>) {
        let payload = &packet[IPV4_MIN_HEADER_LEN..];
        assert_eq!(first_payload_len % 8, 0);
        assert!(first_payload_len < payload.len());
        let build = |fragment_payload: &[u8], offset: usize, more: bool| {
            let mut fragment = vec![0_u8; IPV4_MIN_HEADER_LEN + fragment_payload.len()];
            let fragment_len = fragment.len();
            fragment[..IPV4_MIN_HEADER_LEN].copy_from_slice(&packet[..IPV4_MIN_HEADER_LEN]);
            fragment[2..4].copy_from_slice(
                &u16::try_from(fragment_len)
                    .expect("IPv4 fragment length")
                    .to_be_bytes(),
            );
            let fragment_field = u16::try_from(offset / 8).expect("IPv4 fragment offset")
                | if more { 0x2000 } else { 0 };
            fragment[6..8].copy_from_slice(&fragment_field.to_be_bytes());
            fragment[IPV4_MIN_HEADER_LEN..].copy_from_slice(fragment_payload);
            write_ipv4_header_checksum(&mut fragment, IPV4_MIN_HEADER_LEN);
            fragment
        };
        (
            build(&payload[..first_payload_len], 0, true),
            build(&payload[first_payload_len..], first_payload_len, false),
        )
    }

    fn ipv6_icmp_error(source: Ipv6Addr, destination: Ipv6Addr, quoted: &[u8]) -> Vec<u8> {
        let mut packet = vec![0_u8; IPV6_HEADER_LEN + 8 + quoted.len()];
        packet[0] = 0x60;
        let payload_len = packet.len() - IPV6_HEADER_LEN;
        packet[4..6].copy_from_slice(
            &u16::try_from(payload_len)
                .expect("payload length")
                .to_be_bytes(),
        );
        packet[6] = IP_PROTOCOL_ICMPV6;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet[40] = 1;
        packet[41] = 0;
        packet[48..].copy_from_slice(quoted);
        write_icmpv6_checksum(&mut packet, 40).expect("ICMPv6 checksum");
        packet
    }

    fn fragment_ipv6_packet(packet: &[u8], first_payload_len: usize) -> (Vec<u8>, Vec<u8>) {
        let payload = &packet[IPV6_HEADER_LEN..];
        assert_eq!(first_payload_len % 8, 0);
        assert!(first_payload_len < payload.len());
        let build = |fragment_payload: &[u8], offset: usize, more: bool| {
            let mut fragment = vec![0_u8; IPV6_HEADER_LEN + 8 + fragment_payload.len()];
            fragment[..IPV6_HEADER_LEN].copy_from_slice(&packet[..IPV6_HEADER_LEN]);
            fragment[4..6].copy_from_slice(
                &u16::try_from(8 + fragment_payload.len())
                    .expect("IPv6 fragment payload length")
                    .to_be_bytes(),
            );
            fragment[6] = IP_PROTOCOL_IPV6_FRAGMENT;
            fragment[40] = packet[6];
            let fragment_field =
                u16::try_from(offset / 8).expect("IPv6 fragment offset") << 3 | u16::from(more);
            fragment[42..44].copy_from_slice(&fragment_field.to_be_bytes());
            fragment[44..48].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
            fragment[48..].copy_from_slice(fragment_payload);
            fragment
        };
        (
            build(&payload[..first_payload_len], 0, true),
            build(&payload[first_payload_len..], first_payload_len, false),
        )
    }

    fn ipv6_fragmented_icmp_error(
        source: Ipv6Addr,
        destination: Ipv6Addr,
        quoted: &[u8],
        fragment_field: u16,
    ) -> Vec<u8> {
        let mut packet = vec![0_u8; IPV6_HEADER_LEN + 8 + 8 + quoted.len()];
        packet[0] = 0x60;
        let payload_len = packet.len() - IPV6_HEADER_LEN;
        packet[4..6].copy_from_slice(
            &u16::try_from(payload_len)
                .expect("payload length")
                .to_be_bytes(),
        );
        packet[6] = IP_PROTOCOL_IPV6_FRAGMENT;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet[40] = IP_PROTOCOL_ICMPV6;
        packet[42..44].copy_from_slice(&fragment_field.to_be_bytes());
        packet[48] = 1;
        packet[49] = 0;
        packet[56..].copy_from_slice(quoted);
        write_icmpv6_checksum(&mut packet, 48).expect("ICMPv6 checksum");
        packet
    }

    fn write_tcp_udp_v4_checksum(packet: &mut [u8], offset: usize, checksum_field: usize) {
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[12..20]);
        pseudo.push(0);
        pseudo.push(packet[9]);
        pseudo.extend_from_slice(
            &u16::try_from(packet.len() - offset)
                .expect("segment length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&packet[offset..]);
        pseudo[12 + checksum_field] = 0;
        pseudo[12 + checksum_field + 1] = 0;
        let checksum = internet_checksum(&pseudo);
        packet[offset + checksum_field..offset + checksum_field + 2]
            .copy_from_slice(&checksum.to_be_bytes());
    }

    fn write_udp_v6_checksum(packet: &mut [u8], offset: usize) {
        packet[offset + 6] = 0;
        packet[offset + 7] = 0;
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[8..40]);
        pseudo.extend_from_slice(
            &u32::try_from(packet.len() - offset)
                .expect("segment length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&[0, 0, 0, IP_PROTOCOL_UDP]);
        pseudo.extend_from_slice(&packet[offset..]);
        let checksum = internet_checksum(&pseudo);
        packet[offset + 6..offset + 8].copy_from_slice(&checksum.to_be_bytes());
    }

    fn assert_ipv4_checksum(packet: &[u8]) {
        assert_eq!(internet_checksum(&packet[..20]), 0);
    }

    fn assert_tcp_v4_checksum(packet: &[u8]) {
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[12..20]);
        pseudo.push(0);
        pseudo.push(IP_PROTOCOL_TCP);
        pseudo.extend_from_slice(
            &u16::try_from(packet.len() - 20)
                .expect("segment length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&packet[20..]);
        assert_eq!(internet_checksum(&pseudo), 0);
    }

    fn assert_udp_v4_checksum(packet: &[u8]) {
        let header_len = usize::from(packet[0] & 0x0f) * 4;
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[12..20]);
        pseudo.push(0);
        pseudo.push(IP_PROTOCOL_UDP);
        pseudo.extend_from_slice(
            &u16::try_from(packet.len() - header_len)
                .expect("UDP segment length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&packet[header_len..]);
        assert_eq!(internet_checksum(&pseudo), 0);
    }

    fn assert_udp_v6_checksum(packet: &[u8], offset: usize) {
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[8..40]);
        pseudo.extend_from_slice(
            &u32::try_from(packet.len() - offset)
                .expect("segment length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&[0, 0, 0, IP_PROTOCOL_UDP]);
        pseudo.extend_from_slice(&packet[offset..]);
        assert_eq!(internet_checksum(&pseudo), 0);
    }

    fn assert_icmpv6_checksum(packet: &[u8], offset: usize) {
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[8..40]);
        pseudo.extend_from_slice(
            &u32::try_from(packet.len() - offset)
                .expect("message length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&[0, 0, 0, IP_PROTOCOL_ICMPV6]);
        pseudo.extend_from_slice(&packet[offset..]);
        assert_eq!(internet_checksum(&pseudo), 0);
    }

    fn assert_icmpv6_message_checksum(source: Ipv6Addr, destination: Ipv6Addr, message: &[u8]) {
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&source.octets());
        pseudo.extend_from_slice(&destination.octets());
        pseudo.extend_from_slice(
            &u32::try_from(message.len())
                .expect("ICMPv6 message length")
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(&[0, 0, 0, IP_PROTOCOL_ICMPV6]);
        pseudo.extend_from_slice(message);
        assert_eq!(internet_checksum(&pseudo), 0);
    }
}
