package org.hermeticfoundation.p2pvpn;

import java.util.concurrent.atomic.AtomicReference;

final class ProfileJoinDiscoveryHints {
    static final int MAX_PEER_ID_LENGTH = 256;
    static final int MAX_ADDRESS_LENGTH = 1_024;

    private static final AtomicReference<String> NEXT_DEBUG_HINTS =
            new AtomicReference<>("[]");

    private ProfileJoinDiscoveryHints() {}

    static void setNextForDebug(String peerId, String address) {
        String normalizedPeerId = bounded(peerId, MAX_PEER_ID_LENGTH, "peer_id");
        String normalizedAddress = bounded(address, MAX_ADDRESS_LENGTH, "address");
        NEXT_DEBUG_HINTS.set(
                "[{\"id\":"
                        + JsonStrings.quote(normalizedPeerId)
                        + ",\"address\":"
                        + JsonStrings.quote(normalizedAddress)
                        + "}]");
    }

    static String consumeNextForDebug() {
        return NEXT_DEBUG_HINTS.getAndSet("[]");
    }

    private static String bounded(String value, int maximumLength, String name) {
        if (value == null) {
            throw new IllegalArgumentException("invalid_" + name);
        }
        for (int index = 0; index < value.length(); index++) {
            if (Character.isISOControl(value.charAt(index))) {
                throw new IllegalArgumentException("invalid_" + name);
            }
        }
        String normalized = value.trim();
        if (normalized.isEmpty() || normalized.length() > maximumLength) {
            throw new IllegalArgumentException("invalid_" + name);
        }
        return normalized;
    }
}
