import org.junit.jupiter.api.BeforeEach;

class JournalTest {
    private Journal journal;

    @BeforeEach
    void setUp() {
        journal = JournalFixtures.emptyJournal();
    }
}
