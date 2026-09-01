package org.hermeticfoundation.p2pvpn;

import android.content.Context;
import android.os.Build;
import android.provider.Settings;
import java.text.Normalizer;

final class DeviceHostname {
    static final String DEFAULT_HOSTNAME = "android-device";
    private static final int MAX_LABEL_BYTES = 63;

    private DeviceHostname() {}

    static String resolve(Context context) {
        String deviceName = null;
        if (context != null) {
            try {
                deviceName =
                        Settings.Global.getString(
                                context.getContentResolver(), Settings.Global.DEVICE_NAME);
            } catch (RuntimeException ignored) {
                // Some Android builds restrict access to the user-visible device name.
            }
        }
        return fromCandidates(deviceName, Build.MANUFACTURER, Build.MODEL);
    }

    static String fromCandidates(String deviceName, String manufacturer, String model) {
        String normalizedDeviceName = normalizeOrNull(deviceName);
        if (normalizedDeviceName != null) {
            return normalizedDeviceName;
        }

        return normalize(buildLabel(manufacturer, model));
    }

    static String normalize(String value) {
        String normalized = normalizeOrNull(value);
        return normalized == null ? DEFAULT_HOSTNAME : normalized;
    }

    static String buildLabel(String manufacturer, String model) {
        String usableManufacturer = usableBuildValue(manufacturer);
        String usableModel = usableBuildValue(model);
        if (usableManufacturer == null) {
            return usableModel;
        }
        if (usableModel == null) {
            return usableManufacturer;
        }

        String normalizedManufacturer = normalizeOrNull(usableManufacturer);
        String normalizedModel = normalizeOrNull(usableModel);
        if (normalizedManufacturer != null
                && normalizedModel != null
                && (normalizedModel.equals(normalizedManufacturer)
                        || normalizedModel.startsWith(normalizedManufacturer + "-"))) {
            return usableModel;
        }
        return usableManufacturer + " " + usableModel;
    }

    private static String normalizeOrNull(String value) {
        if (value == null) {
            return null;
        }

        String decomposed = Normalizer.normalize(value, Normalizer.Form.NFKD);
        StringBuilder label = new StringBuilder(Math.min(decomposed.length(), MAX_LABEL_BYTES));
        boolean separatorPending = false;
        for (int index = 0; index < decomposed.length(); index++) {
            char character = decomposed.charAt(index);
            char normalizedCharacter;
            if (character >= 'A' && character <= 'Z') {
                normalizedCharacter = (char) (character + ('a' - 'A'));
            } else if ((character >= 'a' && character <= 'z')
                    || (character >= '0' && character <= '9')) {
                normalizedCharacter = character;
            } else if (isDiscardedMark(character)) {
                continue;
            } else {
                separatorPending = label.length() > 0;
                continue;
            }

            if (separatorPending && label.length() < MAX_LABEL_BYTES) {
                label.append('-');
            }
            separatorPending = false;
            if (label.length() < MAX_LABEL_BYTES) {
                label.append(normalizedCharacter);
            }
            if (label.length() == MAX_LABEL_BYTES) {
                break;
            }
        }

        while (label.length() > 0 && label.charAt(label.length() - 1) == '-') {
            label.setLength(label.length() - 1);
        }
        return label.length() == 0 ? null : label.toString();
    }

    private static boolean isDiscardedMark(char character) {
        int type = Character.getType(character);
        return character == '\''
                || character == '\u2019'
                || type == Character.NON_SPACING_MARK
                || type == Character.COMBINING_SPACING_MARK
                || type == Character.ENCLOSING_MARK;
    }

    private static String usableBuildValue(String value) {
        if (value == null) {
            return null;
        }
        String trimmed = value.trim();
        if (trimmed.isEmpty() || "unknown".equalsIgnoreCase(trimmed)) {
            return null;
        }
        return trimmed;
    }
}
