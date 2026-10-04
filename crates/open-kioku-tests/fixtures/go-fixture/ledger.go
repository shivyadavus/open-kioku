package main

func balance(entries []int) int {
    total := 0
    for _, entry := range entries {
        total += entry
    }
    return total
}
