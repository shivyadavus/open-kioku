package main

import "example.com/app/store"

func Run() int {
	e := store.Entry{Amount: 1}
	return e.Amount
}
