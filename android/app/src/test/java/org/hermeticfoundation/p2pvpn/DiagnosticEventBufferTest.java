package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import org.junit.Test;

public final class DiagnosticEventBufferTest {
    @Test
    public void historyIsBoundedAndReportsDiscardedEvents() {
        DiagnosticEventBuffer events = new DiagnosticEventBuffer();
        for (int index = 0; index < DiagnosticEventBuffer.CAPACITY + 3; index++) {
            events.record("runtime_started", index);
        }

        DiagnosticEventBuffer.Snapshot snapshot = events.snapshot();
        assertEquals(DiagnosticEventBuffer.CAPACITY, snapshot.entries.size());
        assertEquals(3, snapshot.discarded);
        assertEquals(4, snapshot.entries.get(0).sequence);
        assertEquals(
                DiagnosticEventBuffer.CAPACITY + 3,
                snapshot.entries.get(snapshot.entries.size() - 1).sequence);
    }

    @Test
    public void eventNamesCannotCarryFreeFormSensitiveData() {
        DiagnosticEventBuffer events = new DiagnosticEventBuffer();

        assertThrows(
                IllegalArgumentException.class,
                () -> events.record("peer=12D3KooWsecret", 1));
        assertThrows(IllegalArgumentException.class, () -> events.record("underlay changed", 1));
    }
}
