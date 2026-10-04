package testutil

type Server struct {
    Entries []int
}

func NewServer() *Server {
    return &Server{}
}

func TestServer() *Server {
    return NewServer()
}
