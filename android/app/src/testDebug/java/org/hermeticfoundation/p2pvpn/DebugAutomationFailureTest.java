package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;

import org.junit.Test;

public final class DebugAutomationFailureTest {
    @Test
    public void excludesMessagesCausesAndFilenames() {
        RuntimeException error = new IllegalStateException("secret", new Exception("private"));
        error.setStackTrace(new StackTraceElement[] {
                new StackTraceElement("Example", "status", "secret-file", 42)
        });
        assertEquals("java.lang.IllegalStateException at Example.status:42",
                DebugAutomationFailure.describe(error));
    }

    @Test
    public void boundsFramesAndLength() {
        RuntimeException error = new RuntimeException("secret");
        StackTraceElement frame = new StackTraceElement("Example", "status", "private", 1);
        error.setStackTrace(new StackTraceElement[] {
                frame, frame, frame, frame,
                new StackTraceElement("Excluded", "status", "private", 2)
        });
        assertFalse(DebugAutomationFailure.describe(error).contains("Excluded"));
        error.setStackTrace(new StackTraceElement[] {
                new StackTraceElement("a".repeat(2000), "status", "private", 1)
        });
        assertEquals(1024, DebugAutomationFailure.describe(error).length());
    }
}
