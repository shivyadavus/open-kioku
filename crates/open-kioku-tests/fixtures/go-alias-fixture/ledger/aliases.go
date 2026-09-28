package ledger

import (
	"io"

	"example.com/app/store"
)

type Entry = store.Entry

type (
	Batch  = store.Batch
	Source = store.Source
	Reader = io.Reader
	Raw    = []byte
)
