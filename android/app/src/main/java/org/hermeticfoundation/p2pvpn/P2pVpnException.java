package org.hermeticfoundation.p2pvpn;

final class P2pVpnException extends Exception {
    P2pVpnException(String message) {
        super(message);
    }

    P2pVpnException(String message, Throwable cause) {
        super(message, cause);
    }
}
