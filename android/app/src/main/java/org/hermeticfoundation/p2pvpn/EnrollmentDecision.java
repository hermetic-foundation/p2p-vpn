package org.hermeticfoundation.p2pvpn;

final class EnrollmentDecision {
    enum Action {
        POLL,
        AWAIT_APPROVAL,
        APPLY_ARTIFACTS,
        TERMINAL
    }

    private EnrollmentDecision() {}

    static Action evaluate(String phase, boolean artifactsReady, boolean hasCandidate) {
        if (artifactsReady) {
            return Action.APPLY_ARTIFACTS;
        }
        if ("awaiting_approval".equals(phase) && hasCandidate) {
            return Action.AWAIT_APPROVAL;
        }
        if ("rejected".equals(phase)
                || "cancelled".equals(phase)
                || "expired".equals(phase)
                || "failed".equals(phase)
                || "completed".equals(phase)) {
            return Action.TERMINAL;
        }
        return Action.POLL;
    }
}
