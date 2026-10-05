package journal

import (
    "os"
    "testing"
)

func TestMain(m *testing.M) {
    os.Exit(m.Run())
}

func newJournal() []int {
    return []int{}
}
