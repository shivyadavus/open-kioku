package com.acme;

public class Ledger {
    private long total;

    public long post(long amount) {
        total += amount;
        return total;
    }
}
