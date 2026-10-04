public class Publisher {
    public void publishCreated(KafkaTemplate kafkaTemplate, String key) {
        kafkaTemplate.send("entry.created", key);
    }
}
