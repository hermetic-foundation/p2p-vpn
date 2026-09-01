package org.hermeticfoundation.p2pvpn;

import java.net.Inet4Address;
import java.net.Inet6Address;
import java.net.InetAddress;
import java.net.UnknownHostException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.UUID;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class ProfileCollection {
    static final String KIND = "p2p-vpn-android-profile-collection";
    static final int SCHEMA_VERSION = 2;
    static final int MAX_NETWORKS = 16;
    static final int MAX_CONFIG_BYTES = 2 * 1024 * 1024;
    static final int MAX_COLLECTION_BYTES = 8 * 1024 * 1024;

    final List<Entry> networks;
    final String selectedNetworkId;
    final PresentationAddresses presentationAddresses;

    private ProfileCollection(
            List<Entry> networks,
            String selectedNetworkId,
            PresentationAddresses presentationAddresses)
            throws P2pVpnException {
        if (presentationAddresses == null) {
            throw new P2pVpnException("Profile collection is missing presentation addresses");
        }
        this.networks = validateNetworks(networks, selectedNetworkId);
        this.selectedNetworkId = selectedNetworkId;
        this.presentationAddresses = presentationAddresses;
    }

    static Decoded decode(String stored) throws P2pVpnException {
        requireBoundedValue(stored, MAX_COLLECTION_BYTES, "stored profile collection");
        try {
            JSONObject value = new JSONObject(stored);
            if (!KIND.equals(value.optString("kind"))) {
                return Decoded.legacy(stored);
            }
            int schemaVersion = value.getInt("schema_version");
            if (schemaVersion != 1 && schemaVersion != SCHEMA_VERSION) {
                throw new P2pVpnException("Stored profile collection has an unsupported schema");
            }
            List<Entry> networks = decodeNetworks(value.getJSONArray("networks"));
            String selectedNetworkId = value.getString("selected_network_id");
            if (schemaVersion == 1) {
                return Decoded.schemaV1(new SchemaV1Collection(networks, selectedNetworkId));
            }
            PresentationAddresses presentationAddresses =
                    PresentationAddresses.fromJson(value.getJSONObject("presentation_addresses"));
            return Decoded.current(
                    new ProfileCollection(networks, selectedNetworkId, presentationAddresses));
        } catch (JSONException error) {
            throw new P2pVpnException("Stored profile collection is malformed", error);
        }
    }

    static ProfileCollection migrated(
            String configJson,
            String networkName,
            String peerId,
            PresentationAddresses presentationAddresses)
            throws P2pVpnException {
        String id = migratedNetworkId(networkName, peerId);
        return new ProfileCollection(
                Collections.singletonList(new Entry(id, true, configJson)),
                id,
                presentationAddresses);
    }

    static ProfileCollection single(Entry network, PresentationAddresses presentationAddresses)
            throws P2pVpnException {
        return new ProfileCollection(
                Collections.singletonList(network), network.id, presentationAddresses);
    }

    static String newNetworkId() {
        return UUID.randomUUID().toString();
    }

    static String migratedNetworkId(String networkName, String peerId) throws P2pVpnException {
        String normalizedName = requireIdentityValue(networkName, "network name");
        String normalizedPeer = requireIdentityValue(peerId, "peer ID");
        String seed = KIND + "\u0000" + normalizedName + "\u0000" + normalizedPeer;
        return UUID.nameUUIDFromBytes(seed.getBytes(StandardCharsets.UTF_8)).toString();
    }

    Entry selected() {
        return require(selectedNetworkId);
    }

    Entry find(String networkId) {
        for (Entry network : networks) {
            if (network.id.equals(networkId)) {
                return network;
            }
        }
        return null;
    }

    ProfileCollection add(Entry network, boolean select) throws P2pVpnException {
        if (find(network.id) != null) {
            throw new P2pVpnException("A network with this ID already exists");
        }
        List<Entry> updated = new ArrayList<>(networks);
        updated.add(network);
        return new ProfileCollection(
                updated, select ? network.id : selectedNetworkId, presentationAddresses);
    }

    ProfileCollection replace(Entry network) throws P2pVpnException {
        List<Entry> updated = new ArrayList<>(networks);
        for (int index = 0; index < updated.size(); index++) {
            if (updated.get(index).id.equals(network.id)) {
                updated.set(index, network);
                return new ProfileCollection(updated, selectedNetworkId, presentationAddresses);
            }
        }
        throw new P2pVpnException("Cannot update an unknown network");
    }

    ProfileCollection select(String networkId) throws P2pVpnException {
        if (find(networkId) == null) {
            throw new P2pVpnException("Cannot select an unknown network");
        }
        return new ProfileCollection(networks, networkId, presentationAddresses);
    }

    ProfileCollection remove(String networkId) throws P2pVpnException {
        if (networks.size() == 1) {
            throw new P2pVpnException("Cannot create an empty profile collection");
        }
        List<Entry> updated = new ArrayList<>(networks);
        if (!updated.removeIf(network -> network.id.equals(networkId))) {
            throw new P2pVpnException("Cannot remove an unknown network");
        }
        String selected =
                selectedNetworkId.equals(networkId) ? updated.get(0).id : selectedNetworkId;
        return new ProfileCollection(updated, selected, presentationAddresses);
    }

    String toJson() throws P2pVpnException {
        try {
            JSONObject value = new JSONObject();
            value.put("schema_version", SCHEMA_VERSION);
            value.put("kind", KIND);
            value.put("selected_network_id", selectedNetworkId);
            value.put("presentation_addresses", presentationAddresses.toJson());
            JSONArray encodedNetworks = new JSONArray();
            for (Entry network : networks) {
                JSONObject encoded = new JSONObject();
                encoded.put("id", network.id);
                encoded.put("enabled", network.enabled);
                encoded.put("config_json", network.configJson);
                encodedNetworks.put(encoded);
            }
            value.put("networks", encodedNetworks);
            String encoded = value.toString();
            requireBoundedValue(encoded, MAX_COLLECTION_BYTES, "profile collection");
            return encoded;
        } catch (JSONException error) {
            throw new P2pVpnException("Failed to encode profile collection", error);
        }
    }

    private static List<Entry> decodeNetworks(JSONArray encodedNetworks)
            throws JSONException, P2pVpnException {
        List<Entry> networks = new ArrayList<>(encodedNetworks.length());
        for (int index = 0; index < encodedNetworks.length(); index++) {
            JSONObject encoded = encodedNetworks.getJSONObject(index);
            networks.add(
                    new Entry(
                            encoded.getString("id"),
                            encoded.getBoolean("enabled"),
                            encoded.getString("config_json")));
        }
        return networks;
    }

    private static List<Entry> validateNetworks(List<Entry> networks, String selectedNetworkId)
            throws P2pVpnException {
        if (networks == null || networks.isEmpty() || networks.size() > MAX_NETWORKS) {
            throw new P2pVpnException(
                    "A profile collection must contain between 1 and 16 networks");
        }
        List<Entry> copy = new ArrayList<>(networks.size());
        Set<String> identifiers = new HashSet<>();
        for (Entry network : networks) {
            if (network == null || !identifiers.add(network.id)) {
                throw new P2pVpnException("Profile collection contains a duplicate network ID");
            }
            copy.add(network);
        }
        if (selectedNetworkId == null || !identifiers.contains(selectedNetworkId)) {
            throw new P2pVpnException("Profile collection selects an unknown network");
        }
        return Collections.unmodifiableList(copy);
    }

    private Entry require(String networkId) {
        Entry network = find(networkId);
        if (network == null) {
            throw new IllegalStateException("Profile collection invariant is invalid");
        }
        return network;
    }

    private static String requireIdentityValue(String value, String label) throws P2pVpnException {
        if (value == null || value.trim().isEmpty()) {
            throw new P2pVpnException("Cannot derive migrated network ID without " + label);
        }
        return value.trim();
    }

    private static void requireBoundedValue(String value, int maximumBytes, String label)
            throws P2pVpnException {
        if (value == null) {
            throw new P2pVpnException("Missing " + label);
        }
        int length = value.getBytes(StandardCharsets.UTF_8).length;
        if (length == 0 || length > maximumBytes) {
            throw new P2pVpnException("Invalid " + label + " size");
        }
    }

    static final class Entry {
        final String id;
        final boolean enabled;
        final String configJson;

        Entry(String id, boolean enabled, String configJson) throws P2pVpnException {
            this.id = normalizeNetworkId(id);
            requireBoundedValue(configJson, MAX_CONFIG_BYTES, "network profile");
            this.enabled = enabled;
            this.configJson = configJson;
        }

        Entry withEnabled(boolean enabled) throws P2pVpnException {
            return new Entry(id, enabled, configJson);
        }

        Entry withConfig(String configJson) throws P2pVpnException {
            return new Entry(id, enabled, configJson);
        }

        static String normalizeNetworkId(String value) throws P2pVpnException {
            if (value == null) {
                throw new P2pVpnException("Network ID is missing");
            }
            try {
                String normalized = UUID.fromString(value).toString().toLowerCase(Locale.ROOT);
                if (!normalized.equals(value)) {
                    throw new P2pVpnException("Network ID is not canonical");
                }
                return normalized;
            } catch (IllegalArgumentException error) {
                throw new P2pVpnException("Network ID is invalid", error);
            }
        }
    }

    static final class PresentationAddresses {
        static final int IPV4_PREFIX_LENGTH = 32;
        static final int IPV6_PREFIX_LENGTH = 128;

        final String ipv4Address;
        final String ipv6Address;

        private PresentationAddresses(String ipv4Address, String ipv6Address) {
            this.ipv4Address = ipv4Address;
            this.ipv6Address = ipv6Address;
        }

        static PresentationAddresses of(String ipv4Address, String ipv6Address)
                throws P2pVpnException {
            return new PresentationAddresses(
                    requireCanonicalIpv4(ipv4Address), requireCanonicalIpv6(ipv6Address));
        }

        static PresentationAddresses fromProfile(AndroidProfile profile) throws P2pVpnException {
            String ipv4Address = null;
            String ipv6Address = null;
            for (AndroidProfile.Cidr address : profile.addresses) {
                if (ipv4Address == null && address.inetAddress instanceof Inet4Address) {
                    ipv4Address = canonicalIpv4(address.inetAddress.getAddress());
                } else if (ipv6Address == null && address.inetAddress instanceof Inet6Address) {
                    ipv6Address = canonicalIpv6(address.inetAddress.getAddress());
                }
            }
            if (ipv4Address == null || ipv6Address == null) {
                throw new P2pVpnException(
                        "Selected profile must contain IPv4 and IPv6 presentation addresses");
            }
            return new PresentationAddresses(ipv4Address, ipv6Address);
        }

        private static PresentationAddresses fromJson(JSONObject value)
                throws JSONException, P2pVpnException {
            JSONObject ipv4 = value.getJSONObject("ipv4");
            JSONObject ipv6 = value.getJSONObject("ipv6");
            if (ipv4.getInt("prefix_length") != IPV4_PREFIX_LENGTH
                    || ipv6.getInt("prefix_length") != IPV6_PREFIX_LENGTH) {
                throw new P2pVpnException("Stored presentation address prefix is invalid");
            }
            return of(ipv4.getString("address"), ipv6.getString("address"));
        }

        private JSONObject toJson() throws JSONException {
            JSONObject value = new JSONObject();
            value.put("ipv4", encodedAddress(ipv4Address, IPV4_PREFIX_LENGTH));
            value.put("ipv6", encodedAddress(ipv6Address, IPV6_PREFIX_LENGTH));
            return value;
        }

        private static JSONObject encodedAddress(String address, int prefixLength)
                throws JSONException {
            JSONObject value = new JSONObject();
            value.put("address", address);
            value.put("prefix_length", prefixLength);
            return value;
        }

        private static String requireCanonicalIpv4(String value) throws P2pVpnException {
            if (value == null) {
                throw new P2pVpnException("Stored IPv4 presentation address is missing");
            }
            String[] octets = value.split("\\.", -1);
            if (octets.length != 4) {
                throw new P2pVpnException("Stored IPv4 presentation address is invalid");
            }
            byte[] bytes = new byte[4];
            for (int index = 0; index < octets.length; index++) {
                String octet = octets[index];
                if (octet.isEmpty() || (octet.length() > 1 && octet.charAt(0) == '0')) {
                    throw new P2pVpnException("Stored IPv4 presentation address is not canonical");
                }
                int numeric = 0;
                for (int characterIndex = 0; characterIndex < octet.length(); characterIndex++) {
                    char character = octet.charAt(characterIndex);
                    if (character < '0' || character > '9') {
                        throw new P2pVpnException("Stored IPv4 presentation address is invalid");
                    }
                    numeric = numeric * 10 + character - '0';
                    if (numeric > 255) {
                        throw new P2pVpnException("Stored IPv4 presentation address is invalid");
                    }
                }
                bytes[index] = (byte) numeric;
            }
            String canonical = canonicalIpv4(bytes);
            if (!canonical.equals(value)) {
                throw new P2pVpnException("Stored IPv4 presentation address is not canonical");
            }
            return canonical;
        }

        private static String requireCanonicalIpv6(String value) throws P2pVpnException {
            if (value == null || value.isEmpty() || value.indexOf(':') < 0) {
                throw new P2pVpnException("Stored IPv6 presentation address is invalid");
            }
            for (int index = 0; index < value.length(); index++) {
                char character = value.charAt(index);
                boolean hexadecimal =
                        (character >= '0' && character <= '9')
                                || (character >= 'a' && character <= 'f')
                                || (character >= 'A' && character <= 'F');
                if (character != ':' && !hexadecimal) {
                    throw new P2pVpnException("Stored IPv6 presentation address is invalid");
                }
            }
            try {
                InetAddress parsed = InetAddress.getByName(value);
                if (!(parsed instanceof Inet6Address)) {
                    throw new P2pVpnException("Stored IPv6 presentation address has wrong family");
                }
                String canonical = canonicalIpv6(parsed.getAddress());
                if (!canonical.equals(value)) {
                    throw new P2pVpnException("Stored IPv6 presentation address is not canonical");
                }
                return canonical;
            } catch (UnknownHostException error) {
                throw new P2pVpnException("Stored IPv6 presentation address is invalid", error);
            }
        }

        private static String canonicalIpv4(byte[] bytes) {
            return (bytes[0] & 0xff)
                    + "."
                    + (bytes[1] & 0xff)
                    + "."
                    + (bytes[2] & 0xff)
                    + "."
                    + (bytes[3] & 0xff);
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

    static final class SchemaV1Collection {
        final List<Entry> networks;
        final String selectedNetworkId;

        private SchemaV1Collection(List<Entry> networks, String selectedNetworkId)
                throws P2pVpnException {
            this.networks = validateNetworks(networks, selectedNetworkId);
            this.selectedNetworkId = selectedNetworkId;
        }

        ProfileCollection migrate(PresentationAddresses presentationAddresses)
                throws P2pVpnException {
            return new ProfileCollection(networks, selectedNetworkId, presentationAddresses);
        }
    }

    abstract static class Decoded {
        enum State {
            CURRENT,
            SCHEMA_V1,
            LEGACY_PROFILE
        }

        final State state;

        private Decoded(State state) {
            this.state = state;
        }

        static Decoded current(ProfileCollection collection) {
            return new CurrentDecoded(collection);
        }

        static Decoded schemaV1(SchemaV1Collection collection) {
            return new SchemaV1Decoded(collection);
        }

        static Decoded legacy(String configJson) {
            return new LegacyDecoded(configJson);
        }

        ProfileCollection currentCollection() {
            throw new IllegalStateException("Decoded profile collection requires migration");
        }

        SchemaV1Collection schemaV1Collection() {
            throw new IllegalStateException("Decoded profile collection is not schema v1");
        }

        String legacyConfigJson() {
            throw new IllegalStateException("Decoded profile collection is not legacy JSON");
        }

        boolean needsMigration() {
            return state != State.CURRENT;
        }

        private static final class CurrentDecoded extends Decoded {
            private final ProfileCollection collection;

            private CurrentDecoded(ProfileCollection collection) {
                super(State.CURRENT);
                this.collection = collection;
            }

            @Override
            ProfileCollection currentCollection() {
                return collection;
            }
        }

        private static final class SchemaV1Decoded extends Decoded {
            private final SchemaV1Collection collection;

            private SchemaV1Decoded(SchemaV1Collection collection) {
                super(State.SCHEMA_V1);
                this.collection = collection;
            }

            @Override
            SchemaV1Collection schemaV1Collection() {
                return collection;
            }
        }

        private static final class LegacyDecoded extends Decoded {
            private final String configJson;

            private LegacyDecoded(String configJson) {
                super(State.LEGACY_PROFILE);
                this.configJson = configJson;
            }

            @Override
            String legacyConfigJson() {
                return configJson;
            }
        }
    }
}
