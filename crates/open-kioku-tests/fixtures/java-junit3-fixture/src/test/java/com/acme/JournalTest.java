package com.acme;

import org.junit.jupiter.api.Test;

class JournalTest {
    @Test
    void replaysEntries() {
        Assertions.assertEquals(2, new Journal().replay(2));
    }
}
