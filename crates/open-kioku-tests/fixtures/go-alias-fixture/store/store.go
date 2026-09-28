package store

type Entry struct {
	Amount int
}

type Batch struct {
	Entries []Entry
}

type Source interface {
	Next() (Entry, bool)
}
