package org.hermeticfoundation.p2pvpn;

final class JsonStrings {
    private JsonStrings() {}

    static String quote(String value) {
        StringBuilder encoded = new StringBuilder(value.length() + 2);
        encoded.append('"');
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            switch (character) {
                case '"':
                    encoded.append("\\\"");
                    break;
                case '\\':
                    encoded.append("\\\\");
                    break;
                case '\b':
                    encoded.append("\\b");
                    break;
                case '\f':
                    encoded.append("\\f");
                    break;
                case '\n':
                    encoded.append("\\n");
                    break;
                case '\r':
                    encoded.append("\\r");
                    break;
                case '\t':
                    encoded.append("\\t");
                    break;
                default:
                    if (character < 0x20) {
                        encoded.append(String.format("\\u%04x", (int) character));
                    } else {
                        encoded.append(character);
                    }
            }
        }
        return encoded.append('"').toString();
    }
}
