package com.acme;

import junit.framework.TestCase;

public abstract class AbstractLedgerTest extends TestCase {
    protected Ledger ledger() {
        return new Ledger();
    }
}
