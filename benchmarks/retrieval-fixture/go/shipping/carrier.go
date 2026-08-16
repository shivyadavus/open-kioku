package shipping

// Carrier provides upstream shipping rates for zones and package weight.
type Carrier interface { Rate(zone string, weight int) int }

type FixedCarrier struct{}
func (FixedCarrier) Rate(zone string, weight int) int { return weight * 125 }
