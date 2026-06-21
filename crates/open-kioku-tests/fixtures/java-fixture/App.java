public class App {
    @GetMapping("/v1/hello")
    public void hello() {
        System.out.println("hello");
    }

    @KafkaListener(topics = "hello.created")
    public void consumeHello(String message) {
        System.out.println(message);
    }

    public void publishHello(KafkaTemplate kafkaTemplate) {
        kafkaTemplate.send("hello.created", "hello");
    }
}
