package main

import "example.com/app/ledger"

func Record() int {
	entry := ledger.Entry{Amount: 1}
	batch := ledger.Batch{}
	var raw ledger.Raw
	return entry.Amount + len(batch.Entries) + len(raw)
}
