package org.hermeticfoundation.p2pvpn;

import java.util.Locale;

final class NetworkUiState {
    enum Kind {
        DISABLED,
        STARTING,
        CONNECTED,
        DEGRADED,
        RECOVERING
    }

    final Kind kind;
    final String detail;

    private NetworkUiState(Kind kind, String detail) {
        this.kind = kind;
        this.detail = detail == null ? "" : detail.trim();
    }

    static NetworkUiState from(
            boolean enabled, String phase, String detail, boolean connectionRequested) {
        if (!enabled) {
            return new NetworkUiState(Kind.DISABLED, "");
        }
        String normalizedPhase = phase == null ? "" : phase.toLowerCase(Locale.ROOT);
        String normalizedDetail = detail == null ? "" : detail.toLowerCase(Locale.ROOT);
        switch (normalizedPhase) {
            case "running":
                return new NetworkUiState(Kind.CONNECTED, detail);
            case "failed":
                return new NetworkUiState(Kind.DEGRADED, detail);
            case "starting":
                if (normalizedDetail.contains("recover")
                        || normalizedDetail.contains("reconnect")
                        || normalizedDetail.contains("retry")) {
                    return new NetworkUiState(Kind.RECOVERING, detail);
                }
                return new NetworkUiState(Kind.STARTING, detail);
            case "stopped":
                return new NetworkUiState(
                        connectionRequested ? Kind.RECOVERING : Kind.DEGRADED, detail);
            default:
                return new NetworkUiState(
                        connectionRequested ? Kind.RECOVERING : Kind.STARTING, detail);
        }
    }
}
