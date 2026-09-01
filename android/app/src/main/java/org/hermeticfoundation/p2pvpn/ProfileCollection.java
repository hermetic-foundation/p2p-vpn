package org.hermeticfoundation.p2pvpn;

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
    static final int SCHEMA_VERSION = 1;
    static final int MAX_NETWORKS = 16;
    static final int MAX_CONFIG_BYTES = 2 * 1024 * 1024;
    static final int MAX_COLLECTION_BYTES = 8 * 1024 * 1024;

    final List<Entry> networks;
    final String selectedNetworkId;

    private ProfileCollection(List<Entry> networks, String selectedNetworkId)
            throws P2pVpnException {
        if (networks.isEmpty() || networks.size() > MAX_NETWORKS) {
            throw new P2pVpnException("A profile collection must contain between 1 and 16 networks");
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
        this.networks = Collections.unmodifiableList(copy);
        this.selectedNetworkId = selectedNetworkId;
    }

    static Decoded decode(String stored) throws P2pVpnException {
        requireBoundedValue(stored, MAX_COLLECTION_BYTES, "stored profile collection");
        try {
            JSONObject value = new JSONObject(stored);
            if (!KIND.equals(value.optString("kind"))) {
                return Decoded.legacy(stored);
            }
            if (value.getInt("schema_version") != SCHEMA_VERSION) {
                throw new P2pVpnException("Stored profile collection has an unsupported schema");
            }
            JSONArray encodedNetworks = value.getJSONArray("networks");
            List<Entry> networks = new ArrayList<>(encodedNetworks.length());
            for (int index = 0; index < encodedNetworks.length(); index++) {
                JSONObject encoded = encodedNetworks.getJSONObject(index);
                networks.add(
                        new Entry(
                                encoded.getString("id"),
                                encoded.getBoolean("enabled"),
                                encoded.getString("config_json")));
            }
            return Decoded.collection(
                    new ProfileCollection(networks, value.getString("selected_network_id")));
        } catch (JSONException error) {
            throw new P2pVpnException("Stored profile collection is malformed", error);
        }
    }

    static ProfileCollection migrated(String configJson, String networkName, String peerId)
            throws P2pVpnException {
        String id = migratedNetworkId(networkName, peerId);
        return new ProfileCollection(
                Collections.singletonList(new Entry(id, true, configJson)), id);
    }

    static ProfileCollection single(Entry network) throws P2pVpnException {
        return new ProfileCollection(Collections.singletonList(network), network.id);
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
        return new ProfileCollection(updated, select ? network.id : selectedNetworkId);
    }

    ProfileCollection replace(Entry network) throws P2pVpnException {
        List<Entry> updated = new ArrayList<>(networks);
        for (int index = 0; index < updated.size(); index++) {
            if (updated.get(index).id.equals(network.id)) {
                updated.set(index, network);
                return new ProfileCollection(updated, selectedNetworkId);
            }
        }
        throw new P2pVpnException("Cannot update an unknown network");
    }

    ProfileCollection select(String networkId) throws P2pVpnException {
        if (find(networkId) == null) {
            throw new P2pVpnException("Cannot select an unknown network");
        }
        return new ProfileCollection(networks, networkId);
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
        return new ProfileCollection(updated, selected);
    }

    String toJson() throws P2pVpnException {
        try {
            JSONObject value = new JSONObject();
            value.put("schema_version", SCHEMA_VERSION);
            value.put("kind", KIND);
            value.put("selected_network_id", selectedNetworkId);
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

    static final class Decoded {
        final ProfileCollection collection;
        final String legacyConfigJson;

        private Decoded(ProfileCollection collection, String legacyConfigJson) {
            this.collection = collection;
            this.legacyConfigJson = legacyConfigJson;
        }

        static Decoded collection(ProfileCollection collection) {
            return new Decoded(collection, null);
        }

        static Decoded legacy(String configJson) {
            return new Decoded(null, configJson);
        }

        boolean isLegacy() {
            return legacyConfigJson != null;
        }
    }
}
