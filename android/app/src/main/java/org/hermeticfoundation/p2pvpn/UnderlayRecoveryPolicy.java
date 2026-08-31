package org.hermeticfoundation.p2pvpn;

final class UnderlayRecoveryPolicy {
    private static final int MAX_SIGNAL_FAILURES_BEFORE_RESTART = 3;

    enum FailureAction {
        RETRY_SIGNAL,
        RESTART_RUNTIME
    }

    private int signalFailures;

    FailureAction recordSignalFailure() {
        signalFailures++;
        return signalFailures >= MAX_SIGNAL_FAILURES_BEFORE_RESTART
                ? FailureAction.RESTART_RUNTIME
                : FailureAction.RETRY_SIGNAL;
    }

    void reset() {
        signalFailures = 0;
    }
}
