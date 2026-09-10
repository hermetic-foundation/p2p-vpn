package org.hermeticfoundation.p2pvpn;

final class DebugAutomationFailure {
    private DebugAutomationFailure() {}

    // Never serialize exception messages, causes, filenames or command arguments.
    static String describe(Throwable error) {
        StringBuilder result = new StringBuilder(error.getClass().getName());
        StackTraceElement[] frames = error.getStackTrace();
        for (int index = 0; index < Math.min(4, frames.length); index++) {
            StackTraceElement frame = frames[index];
            result.append(" at ").append(frame.getClassName())
                    .append('.').append(frame.getMethodName())
                    .append(':').append(frame.getLineNumber());
        }
        return result.substring(0, Math.min(1024, result.length()));
    }
}
