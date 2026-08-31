package org.hermeticfoundation.p2pvpn;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

final class DiagnosticEventBuffer {
    static final int CAPACITY = 64;

    static final class Entry {
        final long sequence;
        final long sinceServiceStartMillis;
        final String name;

        Entry(long sequence, long sinceServiceStartMillis, String name) {
            this.sequence = sequence;
            this.sinceServiceStartMillis = sinceServiceStartMillis;
            this.name = name;
        }
    }

    static final class Snapshot {
        final List<Entry> entries;
        final long discarded;

        Snapshot(List<Entry> entries, long discarded) {
            this.entries = Collections.unmodifiableList(entries);
            this.discarded = discarded;
        }
    }

    private final ArrayDeque<Entry> entries = new ArrayDeque<>(CAPACITY);
    private long nextSequence = 1;
    private long discarded;

    synchronized void record(String name, long sinceServiceStartMillis) {
        if (!isSafeName(name)) {
            throw new IllegalArgumentException("Diagnostic event name is invalid");
        }
        if (entries.size() == CAPACITY) {
            entries.removeFirst();
            discarded = increment(discarded);
        }
        entries.addLast(
                new Entry(nextSequence, Math.max(0, sinceServiceStartMillis), name));
        nextSequence = increment(nextSequence);
    }

    synchronized Snapshot snapshot() {
        return new Snapshot(new ArrayList<>(entries), discarded);
    }

    private static boolean isSafeName(String name) {
        if (name == null || name.isEmpty() || name.length() > 64) {
            return false;
        }
        for (int index = 0; index < name.length(); index++) {
            char character = name.charAt(index);
            if ((character < 'a' || character > 'z')
                    && (character < '0' || character > '9')
                    && character != '_') {
                return false;
            }
        }
        return true;
    }

    private static long increment(long value) {
        return value == Long.MAX_VALUE ? value : value + 1;
    }
}
