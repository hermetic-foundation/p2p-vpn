package org.hermeticfoundation.p2pvpn;

import java.util.Collection;
import org.json.JSONArray;

final class ProfileJoinRequest {
    static final int MAX_PAIRING_CODE_LENGTH = 64;
    static final int MAX_HOSTNAME_LENGTH = 63;

    final String pairingCode;
    final String hostname;
    final String existingNetworkNamesJson;

    private ProfileJoinRequest(
            String pairingCode, String hostname, String existingNetworkNamesJson) {
        this.pairingCode = pairingCode;
        this.hostname = hostname;
        this.existingNetworkNamesJson = existingNetworkNamesJson;
    }

    static ProfileJoinRequest create(
            String pairingCode, String hostname, Collection<String> existingNetworkNames)
            throws P2pVpnException {
        String normalizedCode = requireValue(pairingCode, MAX_PAIRING_CODE_LENGTH, "pairing code");
        String normalizedHostname = requireValue(hostname, MAX_HOSTNAME_LENGTH, "device hostname");
        if (existingNetworkNames == null
                || existingNetworkNames.size() > ProfileCollection.MAX_NETWORKS) {
            throw new P2pVpnException("Existing network list is invalid");
        }

        JSONArray names = new JSONArray();
        for (String networkName : existingNetworkNames) {
            if (networkName == null || networkName.isEmpty()) {
                throw new P2pVpnException("Existing network list is invalid");
            }
            names.put(networkName);
        }
        return new ProfileJoinRequest(normalizedCode, normalizedHostname, names.toString());
    }

    private static String requireValue(String value, int maximumLength, String label)
            throws P2pVpnException {
        String normalized = value == null ? "" : value.trim();
        if (normalized.isEmpty() || normalized.length() > maximumLength) {
            throw new P2pVpnException("Enter a valid " + label);
        }
        for (int index = 0; index < normalized.length(); index++) {
            if (Character.isISOControl(normalized.charAt(index))) {
                throw new P2pVpnException("Enter a valid " + label);
            }
        }
        return normalized;
    }
}
