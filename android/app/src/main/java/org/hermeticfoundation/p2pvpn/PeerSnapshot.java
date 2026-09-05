package org.hermeticfoundation.p2pvpn;

import java.net.Inet6Address;
import java.net.InetAddress;
import java.net.UnknownHostException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Optional;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class PeerSnapshot {
    static final int SCHEMA_VERSION = 1;
    static final int MAX_ENCODED_BYTES = 128 * 1024;
    static final int MAX_PEERS = 128;
    static final int MAX_HOSTNAMES_PER_PEER = 4;
    static final int MAX_IPV4_PER_PEER = 8;
    static final int MAX_IPV6_PER_PEER = 8;
    static final int MAX_PEER_ID_BYTES = 256;
    static final int MAX_HOSTNAME_BYTES = 63;
    private static final int MAX_JSON_CONTAINERS = 4_096;

    final long observedAtUnixSeconds;
    final int totalPeers;
    final int returnedPeers;
    final boolean truncated;
    final List<Peer> peers;

    private PeerSnapshot(
            long observedAtUnixSeconds,
            int totalPeers,
            int returnedPeers,
            boolean truncated,
            List<Peer> peers) {
        this.observedAtUnixSeconds = observedAtUnixSeconds;
        this.totalPeers = totalPeers;
        this.returnedPeers = returnedPeers;
        this.truncated = truncated;
        this.peers = immutableCopy(peers);
    }

    List<Peer> currentPeers() {
        List<Peer> current = new ArrayList<>();
        for (Peer peer : peers) {
            if (peer.isCurrentMember()) {
                current.add(peer);
            }
        }
        return immutableCopy(current);
    }

    static Optional<PeerSnapshot> parseOptional(JSONObject parent, String fieldName)
            throws P2pVpnException {
        if (parent == null || fieldName == null || fieldName.isEmpty()) {
            throw new P2pVpnException("Peer snapshot parent or field name is missing");
        }
        if (!parent.has(fieldName) || parent.isNull(fieldName)) {
            return Optional.empty();
        }

        Object encoded = parent.opt(fieldName);
        if (!(encoded instanceof JSONObject)) {
            throw new P2pVpnException("Peer snapshot is not a JSON object");
        }
        return parseOptionalSnapshot((JSONObject) encoded);
    }

    private static Optional<PeerSnapshot> parseOptionalSnapshot(JSONObject value)
            throws P2pVpnException {
        try {
            int schemaVersion = requireNonNegativeInt(value, "schema_version");
            if (schemaVersion != SCHEMA_VERSION) {
                return Optional.empty();
            }

            rejectSensitiveFields(value);
            requireEncodedSize(value);

            long observedAtUnixSeconds =
                    requireNonNegativeLong(value, "observed_at_unix_seconds");
            int totalPeers = requireNonNegativeInt(value, "total_peers");
            int returnedPeers = requireNonNegativeInt(value, "returned_peers");
            boolean truncated = requireBoolean(value, "truncated");
            JSONArray encodedPeers = requireArray(value, "peers");

            if (returnedPeers > MAX_PEERS || encodedPeers.length() > MAX_PEERS) {
                throw new P2pVpnException("Peer snapshot exceeds the peer limit");
            }
            if (returnedPeers != encodedPeers.length()) {
                throw new P2pVpnException("Peer snapshot returned peer count is inconsistent");
            }
            if (totalPeers < returnedPeers) {
                throw new P2pVpnException("Peer snapshot total peer count is inconsistent");
            }
            if (truncated != (totalPeers > returnedPeers)) {
                throw new P2pVpnException("Peer snapshot truncation flag is inconsistent");
            }

            List<Peer> peers = new ArrayList<>(returnedPeers);
            Set<String> peerIds = new HashSet<>();
            int localPeers = 0;
            for (int index = 0; index < encodedPeers.length(); index++) {
                Object encodedPeer = encodedPeers.get(index);
                if (!(encodedPeer instanceof JSONObject)) {
                    throw new P2pVpnException("Peer snapshot contains a non-object peer");
                }
                Peer peer = Peer.parse((JSONObject) encodedPeer);
                if (!peerIds.add(peer.peerId)) {
                    throw new P2pVpnException("Peer snapshot contains a duplicate peer ID");
                }
                if (peer.local && ++localPeers > 1) {
                    throw new P2pVpnException("Peer snapshot contains multiple local peers");
                }
                peers.add(peer);
            }

            return Optional.of(
                    new PeerSnapshot(
                            observedAtUnixSeconds,
                            totalPeers,
                            returnedPeers,
                            truncated,
                            peers));
        } catch (JSONException error) {
            throw new P2pVpnException("Peer snapshot is malformed", error);
        }
    }

    private static void requireEncodedSize(JSONObject value) throws P2pVpnException {
        String encoded = value.toString();
        if (encoded == null
                || encoded.getBytes(StandardCharsets.UTF_8).length > MAX_ENCODED_BYTES) {
            throw new P2pVpnException("Peer snapshot exceeds the encoded size limit");
        }
    }

    private static void rejectSensitiveFields(Object value)
            throws JSONException, P2pVpnException {
        Deque<Object> pending = new ArrayDeque<>();
        pending.add(value);
        int containers = 0;
        while (!pending.isEmpty()) {
            Object current = pending.removeLast();
            if (++containers > MAX_JSON_CONTAINERS) {
                throw new P2pVpnException("Peer snapshot contains excessive JSON nesting");
            }
            if (current instanceof JSONObject) {
                JSONObject object = (JSONObject) current;
                Iterator<String> keys = object.keys();
                while (keys.hasNext()) {
                    String key = keys.next();
                    if (isSensitiveField(key)) {
                        throw new P2pVpnException(
                                "Peer snapshot contains prohibited topology or secret data");
                    }
                    addContainer(pending, object.get(key));
                }
            } else if (current instanceof JSONArray) {
                JSONArray array = (JSONArray) current;
                for (int index = 0; index < array.length(); index++) {
                    addContainer(pending, array.get(index));
                }
            }
        }
    }

    private static void addContainer(Deque<Object> pending, Object value) {
        if (value instanceof JSONObject || value instanceof JSONArray) {
            pending.add(value);
        }
    }

    private static boolean isSensitiveField(String fieldName) {
        String normalized = fieldName.toLowerCase(Locale.ROOT).replace('-', '_');
        switch (normalized) {
            case "private_key":
            case "private_keys":
            case "private_key_bytes":
            case "public_key":
            case "public_keys":
            case "public_key_bytes":
            case "membership_key":
            case "membership_tag":
            case "pairing_code":
            case "pairing_secret":
            case "secret":
            case "secrets":
            case "signature":
            case "signatures":
            case "member_record":
            case "member_records":
            case "membership_record":
            case "membership_records":
            case "raw_record":
            case "raw_records":
            case "multiaddr":
            case "multiaddrs":
            case "transport_address":
            case "transport_addresses":
            case "relay_id":
            case "relay_ids":
            case "relay_peer":
            case "relay_peer_id":
            case "connection_id":
            case "connection_ids":
            case "endpoint":
            case "endpoints":
            case "issuer":
            case "issuer_peer":
            case "trust_path":
            case "trust_graph":
                return true;
            default:
                return false;
        }
    }

    private static Object requireValue(JSONObject value, String key) throws P2pVpnException {
        if (!value.has(key) || value.isNull(key)) {
            throw new P2pVpnException("Peer snapshot is missing required field " + key);
        }
        return value.opt(key);
    }

    private static long requireNonNegativeLong(JSONObject value, String key)
            throws P2pVpnException {
        Object encoded = requireValue(value, key);
        if (!(encoded instanceof Byte
                || encoded instanceof Short
                || encoded instanceof Integer
                || encoded instanceof Long)) {
            throw new P2pVpnException("Peer snapshot field " + key + " is not an integer");
        }
        long parsed = ((Number) encoded).longValue();
        if (parsed < 0) {
            throw new P2pVpnException("Peer snapshot field " + key + " is negative");
        }
        return parsed;
    }

    private static int requireNonNegativeInt(JSONObject value, String key)
            throws P2pVpnException {
        long parsed = requireNonNegativeLong(value, key);
        if (parsed > Integer.MAX_VALUE) {
            throw new P2pVpnException("Peer snapshot field " + key + " is too large");
        }
        return (int) parsed;
    }

    private static boolean requireBoolean(JSONObject value, String key)
            throws P2pVpnException {
        Object encoded = requireValue(value, key);
        if (!(encoded instanceof Boolean)) {
            throw new P2pVpnException("Peer snapshot field " + key + " is not a boolean");
        }
        return (Boolean) encoded;
    }

    private static String requireString(JSONObject value, String key) throws P2pVpnException {
        Object encoded = requireValue(value, key);
        if (!(encoded instanceof String)) {
            throw new P2pVpnException("Peer snapshot field " + key + " is not a string");
        }
        return (String) encoded;
    }

    private static JSONArray requireArray(JSONObject value, String key) throws P2pVpnException {
        Object encoded = requireValue(value, key);
        if (!(encoded instanceof JSONArray)) {
            throw new P2pVpnException("Peer snapshot field " + key + " is not an array");
        }
        return (JSONArray) encoded;
    }

    private static <T> List<T> immutableCopy(List<T> values) {
        return Collections.unmodifiableList(new ArrayList<>(values));
    }

    enum MembershipSource {
        LOCAL_CONFIGURATION("local_configuration"),
        PEER_CONFIGURATION("peer_configuration"),
        SIGNED_MEMBERSHIP("signed_membership");

        final String wireName;

        MembershipSource(String wireName) {
            this.wireName = wireName;
        }

        static MembershipSource parse(String value) throws P2pVpnException {
            for (MembershipSource source : values()) {
                if (source.wireName.equals(value)) {
                    return source;
                }
            }
            throw new P2pVpnException("Peer snapshot contains an unknown membership source");
        }
    }

    enum MembershipState {
        CONFIGURED("configured"),
        ACTIVE("active"),
        REVOKED("revoked"),
        EXPIRED("expired"),
        INACTIVE("inactive");

        final String wireName;

        MembershipState(String wireName) {
            this.wireName = wireName;
        }

        static MembershipState parse(String value) throws P2pVpnException {
            for (MembershipState state : values()) {
                if (state.wireName.equals(value)) {
                    return state;
                }
            }
            throw new P2pVpnException("Peer snapshot contains an unknown membership state");
        }
    }

    static final class Inviter {
        final String peerId;
        final Optional<String> hostname;

        private Inviter(String peerId, String hostname) {
            this.peerId = peerId;
            this.hostname = Optional.ofNullable(hostname);
        }

        private static Inviter parse(JSONObject value) throws P2pVpnException {
            String peerId = Peer.requirePeerId(requireString(value, "peer_id"));
            String hostname = null;
            if (value.has("hostname") && !value.isNull("hostname")) {
                hostname = requireString(value, "hostname");
                Peer.requireDnsHostname(hostname);
            }
            return new Inviter(peerId, hostname);
        }
    }

    static final class Membership {
        final MembershipState state;
        final Optional<Inviter> effectiveInviter;
        final Optional<Inviter> originalInviter;
        final Optional<Long> admittedAtUnixSeconds;
        final Optional<Long> originalAdmittedAtUnixSeconds;
        final Optional<Long> stateChangedAtUnixSeconds;

        private Membership(
                MembershipState state,
                Inviter effectiveInviter,
                Inviter originalInviter,
                Long admittedAtUnixSeconds,
                Long originalAdmittedAtUnixSeconds,
                Long stateChangedAtUnixSeconds) {
            this.state = state;
            this.effectiveInviter = Optional.ofNullable(effectiveInviter);
            this.originalInviter = Optional.ofNullable(originalInviter);
            this.admittedAtUnixSeconds = Optional.ofNullable(admittedAtUnixSeconds);
            this.originalAdmittedAtUnixSeconds =
                    Optional.ofNullable(originalAdmittedAtUnixSeconds);
            this.stateChangedAtUnixSeconds = Optional.ofNullable(stateChangedAtUnixSeconds);
        }

        private static Membership parse(JSONObject value) throws P2pVpnException {
            return new Membership(
                    MembershipState.parse(requireString(value, "state")),
                    parseOptionalInviter(value, "effective_inviter"),
                    parseOptionalInviter(value, "original_inviter"),
                    parseOptionalLong(value, "admitted_at_unix_seconds"),
                    parseOptionalLong(value, "original_admitted_at_unix_seconds"),
                    parseOptionalLong(value, "state_changed_at_unix_seconds"));
        }

        private static Inviter parseOptionalInviter(JSONObject value, String key)
                throws P2pVpnException {
            if (!value.has(key) || value.isNull(key)) {
                return null;
            }
            Object encoded = value.opt(key);
            if (!(encoded instanceof JSONObject)) {
                throw new P2pVpnException("Peer snapshot membership inviter is not an object");
            }
            return Inviter.parse((JSONObject) encoded);
        }

        private static Long parseOptionalLong(JSONObject value, String key)
                throws P2pVpnException {
            if (!value.has(key) || value.isNull(key)) {
                return null;
            }
            return requireNonNegativeLong(value, key);
        }
    }

    enum ConnectionState {
        LOCAL("local"),
        CONNECTED("connected"),
        CONNECTING("connecting"),
        RECOVERING("recovering"),
        DISCONNECTED("disconnected");

        final String wireName;

        ConnectionState(String wireName) {
            this.wireName = wireName;
        }

        static ConnectionState parse(String value) throws P2pVpnException {
            for (ConnectionState state : values()) {
                if (state.wireName.equals(value)) {
                    return state;
                }
            }
            throw new P2pVpnException("Peer snapshot contains an unknown connection state");
        }
    }

    enum PathKind {
        DIRECT_UDP_DATAGRAM("direct_udp_datagram"),
        DIRECT_QUIC_DATAGRAM("direct_quic_datagram"),
        DIRECT_QUIC_STREAM("direct_quic_stream"),
        DIRECT_TCP_STREAM("direct_tcp_stream"),
        CIRCUIT_RELAY("circuit_relay");

        final String wireName;

        PathKind(String wireName) {
            this.wireName = wireName;
        }

        static PathKind parse(String value) throws P2pVpnException {
            for (PathKind path : values()) {
                if (path.wireName.equals(value)) {
                    return path;
                }
            }
            throw new P2pVpnException("Peer snapshot contains an unknown selected path");
        }
    }

    enum PathOrigin {
        UNKNOWN("unknown"),
        CONFIGURED("configured"),
        MDNS("mdns"),
        KADEMLIA("kademlia"),
        IDENTIFY("identify"),
        RELAY_CIRCUIT("relay_circuit"),
        DCUTR("dcutr"),
        PACKET_PLANE_NEGOTIATION("packet_plane_negotiation");

        final String wireName;

        PathOrigin(String wireName) {
            this.wireName = wireName;
        }

        static PathOrigin parse(String value) throws P2pVpnException {
            for (PathOrigin origin : values()) {
                if (origin.wireName.equals(value)) {
                    return origin;
                }
            }
            throw new P2pVpnException("Peer snapshot contains an unknown path origin");
        }
    }

    static final class Peer {
        final String peerId;
        final List<String> hostnames;
        final List<String> ipv4;
        final List<String> ipv6;
        final boolean local;
        final Optional<Membership> membership;
        final List<MembershipSource> membershipSources;
        final ConnectionState connectionState;
        final Optional<PathKind> selectedPath;
        final Optional<PathOrigin> pathOrigin;

        private Peer(
                String peerId,
                List<String> hostnames,
                List<String> ipv4,
                List<String> ipv6,
                boolean local,
                Membership membership,
                List<MembershipSource> membershipSources,
                ConnectionState connectionState,
                PathKind selectedPath,
                PathOrigin pathOrigin) {
            this.peerId = peerId;
            this.hostnames = immutableCopy(hostnames);
            this.ipv4 = immutableCopy(ipv4);
            this.ipv6 = immutableCopy(ipv6);
            this.local = local;
            this.membership = Optional.ofNullable(membership);
            this.membershipSources = immutableCopy(membershipSources);
            this.connectionState = connectionState;
            this.selectedPath = Optional.ofNullable(selectedPath);
            this.pathOrigin = Optional.ofNullable(pathOrigin);
        }

        private boolean isCurrentMember() {
            if (local) {
                return true;
            }
            if (membership.isPresent()) {
                MembershipState state = membership.get().state;
                return state == MembershipState.CONFIGURED || state == MembershipState.ACTIVE;
            }
            return membershipSources.contains(MembershipSource.PEER_CONFIGURATION);
        }

        private static Peer parse(JSONObject value) throws JSONException, P2pVpnException {
            String peerId = requirePeerId(requireString(value, "peer_id"));
            List<String> hostnames = parseHostnames(requireArray(value, "hostnames"));
            List<String> ipv4 = parseAddresses(requireArray(value, "ipv4"), false);
            List<String> ipv6 = parseAddresses(requireArray(value, "ipv6"), true);
            boolean local = requireBoolean(value, "local");
            Membership membership = parseOptionalMembership(value);
            List<MembershipSource> membershipSources =
                    parseMembershipSources(requireArray(value, "membership_sources"));
            ConnectionState connectionState =
                    ConnectionState.parse(requireString(value, "connection_state"));
            PathKind selectedPath = parseOptionalPath(value);
            PathOrigin pathOrigin = parseOptionalOrigin(value);

            if (local != (connectionState == ConnectionState.LOCAL)) {
                throw new P2pVpnException("Peer snapshot local peer state is inconsistent");
            }
            boolean hasLocalSource =
                    membershipSources.contains(MembershipSource.LOCAL_CONFIGURATION);
            if (local != hasLocalSource) {
                throw new P2pVpnException("Peer snapshot local membership source is inconsistent");
            }
            if ((selectedPath == null) != (pathOrigin == null)) {
                throw new P2pVpnException("Peer snapshot selected path metadata is incomplete");
            }
            if (connectionState == ConnectionState.CONNECTED && selectedPath == null) {
                throw new P2pVpnException("Connected peer snapshot is missing its selected path");
            }
            if (connectionState != ConnectionState.CONNECTED && selectedPath != null) {
                throw new P2pVpnException("Non-connected peer snapshot has a selected path");
            }

            return new Peer(
                    peerId,
                    hostnames,
                    ipv4,
                    ipv6,
                    local,
                    membership,
                    membershipSources,
                    connectionState,
                    selectedPath,
                    pathOrigin);
        }

        private static Membership parseOptionalMembership(JSONObject value)
                throws P2pVpnException {
            if (!value.has("membership") || value.isNull("membership")) {
                return null;
            }
            Object encoded = value.opt("membership");
            if (!(encoded instanceof JSONObject)) {
                throw new P2pVpnException("Peer snapshot membership is not an object");
            }
            return Membership.parse((JSONObject) encoded);
        }

        private static String requirePeerId(String value) throws P2pVpnException {
            int byteLength = value.getBytes(StandardCharsets.UTF_8).length;
            if (byteLength == 0 || byteLength > MAX_PEER_ID_BYTES) {
                throw new P2pVpnException("Peer snapshot contains an invalid peer ID size");
            }
            for (int index = 0; index < value.length(); index++) {
                char character = value.charAt(index);
                if (!((character >= 'a' && character <= 'z')
                        || (character >= 'A' && character <= 'Z')
                        || (character >= '0' && character <= '9'))) {
                    throw new P2pVpnException("Peer snapshot contains an invalid peer ID");
                }
            }
            return value;
        }

        private static List<String> parseHostnames(JSONArray values)
                throws JSONException, P2pVpnException {
            if (values.length() > MAX_HOSTNAMES_PER_PEER) {
                throw new P2pVpnException("Peer snapshot exceeds the hostname limit");
            }
            List<String> result = new ArrayList<>(values.length());
            Set<String> unique = new HashSet<>();
            for (int index = 0; index < values.length(); index++) {
                String hostname = requireArrayString(values, index, "hostname");
                requireDnsHostname(hostname);
                if (!unique.add(hostname)) {
                    throw new P2pVpnException("Peer snapshot contains a duplicate hostname");
                }
                result.add(hostname);
            }
            return result;
        }

        private static void requireDnsHostname(String value) throws P2pVpnException {
            int byteLength = value.getBytes(StandardCharsets.UTF_8).length;
            if (byteLength == 0 || byteLength > MAX_HOSTNAME_BYTES) {
                throw new P2pVpnException("Peer snapshot contains an invalid hostname size");
            }
            if (!value.equals(value.toLowerCase(Locale.ROOT))
                    || value.charAt(0) == '-'
                    || value.charAt(value.length() - 1) == '-') {
                throw new P2pVpnException("Peer snapshot contains a non-canonical hostname");
            }
            for (int index = 0; index < value.length(); index++) {
                char character = value.charAt(index);
                if (!((character >= 'a' && character <= 'z')
                        || (character >= '0' && character <= '9')
                        || character == '-')) {
                    throw new P2pVpnException("Peer snapshot contains an invalid hostname");
                }
            }
        }

        private static List<String> parseAddresses(JSONArray values, boolean ipv6)
                throws JSONException, P2pVpnException {
            int limit = ipv6 ? MAX_IPV6_PER_PEER : MAX_IPV4_PER_PEER;
            if (values.length() > limit) {
                throw new P2pVpnException("Peer snapshot exceeds the IP address limit");
            }
            List<String> result = new ArrayList<>(values.length());
            Set<String> unique = new HashSet<>();
            for (int index = 0; index < values.length(); index++) {
                String encoded = requireArrayString(values, index, "IP address");
                String address = ipv6 ? requireCanonicalIpv6(encoded) : requireCanonicalIpv4(encoded);
                if (!unique.add(address)) {
                    throw new P2pVpnException("Peer snapshot contains a duplicate IP address");
                }
                result.add(address);
            }
            return result;
        }

        private static List<MembershipSource> parseMembershipSources(JSONArray values)
                throws JSONException, P2pVpnException {
            if (values.length() == 0 || values.length() > MembershipSource.values().length) {
                throw new P2pVpnException("Peer snapshot has an invalid membership source count");
            }
            List<MembershipSource> result = new ArrayList<>(values.length());
            Set<MembershipSource> unique = new HashSet<>();
            for (int index = 0; index < values.length(); index++) {
                MembershipSource source =
                        MembershipSource.parse(
                                requireArrayString(values, index, "membership source"));
                if (!unique.add(source)) {
                    throw new P2pVpnException(
                            "Peer snapshot contains a duplicate membership source");
                }
                result.add(source);
            }
            return result;
        }

        private static PathKind parseOptionalPath(JSONObject value) throws P2pVpnException {
            if (!value.has("selected_path") || value.isNull("selected_path")) {
                return null;
            }
            return PathKind.parse(requireString(value, "selected_path"));
        }

        private static PathOrigin parseOptionalOrigin(JSONObject value) throws P2pVpnException {
            if (!value.has("path_origin") || value.isNull("path_origin")) {
                return null;
            }
            return PathOrigin.parse(requireString(value, "path_origin"));
        }

        private static String requireArrayString(JSONArray values, int index, String label)
                throws JSONException, P2pVpnException {
            Object encoded = values.get(index);
            if (!(encoded instanceof String)) {
                throw new P2pVpnException("Peer snapshot " + label + " is not a string");
            }
            return (String) encoded;
        }

        private static String requireCanonicalIpv4(String value) throws P2pVpnException {
            if (value.length() == 0 || value.length() > 15) {
                throw new P2pVpnException("Peer snapshot contains an invalid IPv4 address");
            }
            String[] octets = value.split("\\.", -1);
            if (octets.length != 4) {
                throw new P2pVpnException("Peer snapshot contains an invalid IPv4 address");
            }
            StringBuilder canonical = new StringBuilder();
            for (int index = 0; index < octets.length; index++) {
                String octet = octets[index];
                if (octet.isEmpty() || (octet.length() > 1 && octet.charAt(0) == '0')) {
                    throw new P2pVpnException("Peer snapshot contains a non-canonical IPv4 address");
                }
                int numeric = 0;
                for (int characterIndex = 0; characterIndex < octet.length(); characterIndex++) {
                    char character = octet.charAt(characterIndex);
                    if (character < '0' || character > '9') {
                        throw new P2pVpnException("Peer snapshot contains an invalid IPv4 address");
                    }
                    numeric = numeric * 10 + character - '0';
                    if (numeric > 255) {
                        throw new P2pVpnException("Peer snapshot contains an invalid IPv4 address");
                    }
                }
                if (index > 0) {
                    canonical.append('.');
                }
                canonical.append(numeric);
            }
            if (!canonical.toString().equals(value)) {
                throw new P2pVpnException("Peer snapshot contains a non-canonical IPv4 address");
            }
            return value;
        }

        private static String requireCanonicalIpv6(String value) throws P2pVpnException {
            if (value.isEmpty() || value.length() > 39 || value.indexOf(':') < 0) {
                throw new P2pVpnException("Peer snapshot contains an invalid IPv6 address");
            }
            for (int index = 0; index < value.length(); index++) {
                char character = value.charAt(index);
                boolean hexadecimal =
                        (character >= '0' && character <= '9')
                                || (character >= 'a' && character <= 'f');
                if (character != ':' && !hexadecimal) {
                    throw new P2pVpnException("Peer snapshot contains an invalid IPv6 address");
                }
            }
            try {
                InetAddress parsed = InetAddress.getByName(value);
                if (!(parsed instanceof Inet6Address)) {
                    throw new P2pVpnException("Peer snapshot IPv6 address has the wrong family");
                }
                String canonical = canonicalIpv6(parsed.getAddress());
                if (!canonical.equals(value)) {
                    throw new P2pVpnException("Peer snapshot contains a non-canonical IPv6 address");
                }
                return canonical;
            } catch (UnknownHostException error) {
                throw new P2pVpnException("Peer snapshot contains an invalid IPv6 address", error);
            }
        }

        private static String canonicalIpv6(byte[] bytes) {
            int[] words = new int[8];
            for (int index = 0; index < words.length; index++) {
                words[index] = ((bytes[index * 2] & 0xff) << 8) | (bytes[index * 2 + 1] & 0xff);
            }
            int bestStart = -1;
            int bestLength = 0;
            for (int index = 0; index < words.length; ) {
                if (words[index] != 0) {
                    index++;
                    continue;
                }
                int end = index;
                while (end < words.length && words[end] == 0) {
                    end++;
                }
                int length = end - index;
                if (length > bestLength && length >= 2) {
                    bestStart = index;
                    bestLength = length;
                }
                index = end;
            }
            StringBuilder result = new StringBuilder();
            for (int index = 0; index < words.length; ) {
                if (index == bestStart) {
                    result.append("::");
                    index += bestLength;
                    continue;
                }
                if (result.length() > 0 && result.charAt(result.length() - 1) != ':') {
                    result.append(':');
                }
                result.append(Integer.toHexString(words[index]));
                index++;
            }
            return result.toString();
        }
    }
}
