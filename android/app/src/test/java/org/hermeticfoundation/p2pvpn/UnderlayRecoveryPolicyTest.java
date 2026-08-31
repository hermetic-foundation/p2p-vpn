package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public final class UnderlayRecoveryPolicyTest {
    @Test
    public void repeatedSignalFailuresEventuallyRestartTheRuntime() {
        UnderlayRecoveryPolicy policy = new UnderlayRecoveryPolicy();

        assertEquals(
                UnderlayRecoveryPolicy.FailureAction.RETRY_SIGNAL,
                policy.recordSignalFailure());
        assertEquals(
                UnderlayRecoveryPolicy.FailureAction.RETRY_SIGNAL,
                policy.recordSignalFailure());
        assertEquals(
                UnderlayRecoveryPolicy.FailureAction.RESTART_RUNTIME,
                policy.recordSignalFailure());
    }

    @Test
    public void successfulRecoveryResetsTheFailureBudget() {
        UnderlayRecoveryPolicy policy = new UnderlayRecoveryPolicy();
        policy.recordSignalFailure();
        policy.recordSignalFailure();

        policy.reset();

        assertEquals(
                UnderlayRecoveryPolicy.FailureAction.RETRY_SIGNAL,
                policy.recordSignalFailure());
    }
}
