package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public final class NetworkUiStateTest {
    @Test
    public void disabledDesiredStateNeverImpliesConnectivity() {
        assertEquals(
                NetworkUiState.Kind.DISABLED,
                NetworkUiState.from(false, "running", "stale", true).kind);
    }

    @Test
    public void runtimePhasesMapToDistinctObservedStates() {
        assertEquals(
                NetworkUiState.Kind.CONNECTED,
                NetworkUiState.from(true, "running", "healthy", true).kind);
        assertEquals(
                NetworkUiState.Kind.DEGRADED,
                NetworkUiState.from(true, "failed", "transport failed", true).kind);
        assertEquals(
                NetworkUiState.Kind.STARTING,
                NetworkUiState.from(true, "starting", "discovering", true).kind);
        assertEquals(
                NetworkUiState.Kind.RECOVERING,
                NetworkUiState.from(true, "starting", "Reconnecting", true).kind);
        assertEquals(
                NetworkUiState.Kind.RECOVERING,
                NetworkUiState.from(true, "stopped", "underlay lost", true).kind);
    }

    @Test
    public void enabledButUnrequestedStateIsVisibleAsDegraded() {
        assertEquals(
                NetworkUiState.Kind.DEGRADED,
                NetworkUiState.from(true, "stopped", "not running", false).kind);
    }
}
