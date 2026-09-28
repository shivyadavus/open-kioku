package ledger

import "example.com/app/store"

type Entry = store.Entry

type (
	Batch = store.Batch
	Raw   = []byte
)
