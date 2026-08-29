package org.hermeticfoundation.p2pvpn;

import java.security.SecureRandom;
import java.util.Base64;

final class PairingOperationId {
    private static final int BYTE_LENGTH = 16;
    private static final SecureRandom RANDOM = new SecureRandom();

    private PairingOperationId() {}

    static String generate() {
        byte[] bytes = new byte[BYTE_LENGTH];
        RANDOM.nextBytes(bytes);
        return Base64.getUrlEncoder().withoutPadding().encodeToString(bytes);
    }
}
