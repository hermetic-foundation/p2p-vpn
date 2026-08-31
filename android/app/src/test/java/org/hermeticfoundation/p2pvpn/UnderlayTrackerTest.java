package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class UnderlayTrackerTest {
    @Test
    public void onlySelectedUnderlayTransitionsRequireRuntimeRecovery() {
        assertFalse(UnderlayTracker.Change.INITIAL.requiresRuntimeRecovery());
        assertFalse(UnderlayTracker.Change.UNCHANGED.requiresRuntimeRecovery());
        assertFalse(UnderlayTracker.Change.AVAILABLE_CHANGED.requiresRuntimeRecovery());
        assertTrue(UnderlayTracker.Change.CHANGED.requiresRuntimeRecovery());
        assertTrue(UnderlayTracker.Change.LOST.requiresRuntimeRecovery());
        assertTrue(UnderlayTracker.Change.RECOVERED.requiresRuntimeRecovery());
    }

    @Test
    public void firstPhysicalNetworkInitializesWithoutARecovery() {
        UnderlayTracker tracker = new UnderlayTracker();
        assertEquals(
                UnderlayTracker.Change.INITIAL,
                tracker.observe("network-1", UnderlayTracker.Kind.CELLULAR, true));
        assertEquals(
                UnderlayTracker.Change.UNCHANGED,
                tracker.observe("network-1", UnderlayTracker.Kind.CELLULAR, true));

        UnderlayTracker.Snapshot snapshot = tracker.snapshot();
        assertEquals("cellular", snapshot.kind);
        assertTrue(snapshot.validated);
        assertEquals(1, snapshot.availableNetworks);
        assertEquals(0, snapshot.selectionChanges);
        assertEquals(0, snapshot.recoveries);
    }

    @Test
    public void betterPhysicalNetworkChangesSelection() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("cellular", UnderlayTracker.Kind.CELLULAR, true);
        assertEquals(
                UnderlayTracker.Change.CHANGED,
                tracker.observe("wifi", UnderlayTracker.Kind.WIFI, true));
        assertEquals("wifi", tracker.snapshot().kind);
        assertEquals(1, tracker.snapshot().selectionChanges);
    }

    @Test
    public void lowerPriorityNetworkDoesNotDisplaceCurrentNetwork() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("wifi", UnderlayTracker.Kind.WIFI, true);
        assertEquals(
                UnderlayTracker.Change.AVAILABLE_CHANGED,
                tracker.observe("cellular", UnderlayTracker.Kind.CELLULAR, true));
        assertEquals("wifi", tracker.snapshot().kind);
        assertEquals(2, tracker.snapshot().availableNetworks);
        assertEquals(UnderlayTracker.Change.AVAILABLE_CHANGED, tracker.lost("cellular"));
        assertEquals(1, tracker.snapshot().availableNetworks);
        assertEquals(0, tracker.snapshot().selectedLosses);
    }

    @Test
    public void selectedNetworkValidationChangesRequireRuntimeRecovery() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("wifi", UnderlayTracker.Kind.WIFI, true);

        assertEquals(
                UnderlayTracker.Change.CHANGED,
                tracker.observe("wifi", UnderlayTracker.Kind.WIFI, false));
        assertFalse(tracker.snapshot().validated);
        assertEquals(
                UnderlayTracker.Change.CHANGED,
                tracker.observe("wifi", UnderlayTracker.Kind.WIFI, true));
        assertTrue(tracker.snapshot().validated);
        assertEquals(2, tracker.snapshot().selectionChanges);
    }

    @Test
    public void currentLossPromotesAvailableFallback() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("wifi", UnderlayTracker.Kind.WIFI, true);
        tracker.observe("cellular", UnderlayTracker.Kind.CELLULAR, true);

        assertEquals(UnderlayTracker.Change.CHANGED, tracker.lost("wifi"));
        UnderlayTracker.Snapshot snapshot = tracker.snapshot();
        assertEquals("cellular", snapshot.kind);
        assertEquals(1, snapshot.selectedLosses);
        assertEquals(1, snapshot.selectionChanges);
        assertEquals(0, snapshot.recoveries);
    }

    @Test
    public void totalLossAndReturnAreTrackedWithoutIdentityData() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("wifi-secret-handle", UnderlayTracker.Kind.WIFI, true);
        assertEquals(UnderlayTracker.Change.LOST, tracker.lost("wifi-secret-handle"));

        UnderlayTracker.Snapshot lost = tracker.snapshot();
        assertEquals("none", lost.kind);
        assertFalse(lost.validated);
        assertEquals(0, lost.availableNetworks);
        assertEquals(1, lost.selectedLosses);

        assertEquals(
                UnderlayTracker.Change.RECOVERED,
                tracker.observe("new-cellular-handle", UnderlayTracker.Kind.CELLULAR, true));
        UnderlayTracker.Snapshot recovered = tracker.snapshot();
        assertEquals("cellular", recovered.kind);
        assertEquals(2, recovered.selectionChanges);
        assertEquals(1, recovered.recoveries);
    }

    @Test
    public void unvalidatedHigherTransportDoesNotDisplaceValidatedNetwork() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("cellular", UnderlayTracker.Kind.CELLULAR, true);
        assertEquals(
                UnderlayTracker.Change.AVAILABLE_CHANGED,
                tracker.observe("ethernet", UnderlayTracker.Kind.ETHERNET, false));
        assertEquals("cellular", tracker.snapshot().kind);
    }

    @Test
    public void equalPriorityCallbacksDoNotCauseChurn() {
        UnderlayTracker tracker = new UnderlayTracker();
        tracker.observe("wifi-2", UnderlayTracker.Kind.WIFI, true);
        assertEquals(
                UnderlayTracker.Change.AVAILABLE_CHANGED,
                tracker.observe("wifi-1", UnderlayTracker.Kind.WIFI, true));
        assertEquals("wifi", tracker.snapshot().kind);
        assertEquals(0, tracker.snapshot().selectionChanges);
    }

    @Test
    public void unknownCallbacksCannotChangeSelection() {
        UnderlayTracker tracker = new UnderlayTracker();
        assertEquals(
                UnderlayTracker.Change.UNCHANGED,
                tracker.observe(null, UnderlayTracker.Kind.WIFI, true));
        assertEquals(
                UnderlayTracker.Change.UNCHANGED,
                tracker.observe("", UnderlayTracker.Kind.WIFI, true));
        assertEquals(
                UnderlayTracker.Change.UNCHANGED,
                tracker.observe("invalid", UnderlayTracker.Kind.NONE, true));
        assertEquals(UnderlayTracker.Change.UNCHANGED, tracker.lost("missing"));
        assertEquals("none", tracker.snapshot().kind);
    }
}
