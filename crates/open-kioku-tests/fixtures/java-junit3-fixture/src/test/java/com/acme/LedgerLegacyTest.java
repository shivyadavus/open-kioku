package com.acme;

public class LedgerLegacyTest extends AbstractLedgerTest {
    public void testPostsLegacy() {
        assertEquals(1, new Ledger().post(1));
    }
}
