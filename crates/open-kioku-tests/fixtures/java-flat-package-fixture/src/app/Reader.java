package org.example.app;

import static org.example.Constants.FLAT_KEY;
import static org.example.Constants.keyOf;

public class Reader {
    String read() {
        return FLAT_KEY + keyOf();
    }
}
