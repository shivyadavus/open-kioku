package main

import (
    "os"
    "testing"
)

func TestMain(m *testing.M) {
    os.Exit(m.Run())
}

func LedgerFixture() []int {
    return []int{2, 3}
}

func TestBalanceSumsEntries(t *testing.T) {
    if balance(LedgerFixture()) != 5 {
        t.Fatal("balance")
    }
}
