import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class PublisherTest {
    private Publisher publisher;

    @BeforeEach
    void setUp() {
        publisher = new Publisher();
    }

    private KafkaTemplate recordingTemplate() {
        return new KafkaTemplate();
    }

    @Test
    void publishSendsTheCreatedEvent() {
        publisher.publishCreated(recordingTemplate(), "entry-1");
    }
}
