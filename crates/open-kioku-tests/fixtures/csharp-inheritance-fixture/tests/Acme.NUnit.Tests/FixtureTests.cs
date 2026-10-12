using NUnit.Framework;

namespace Acme.NUnitChecks;

[TestFixture("usd")]
[TestFixture("eur")]
public class CurrencyTests
{
    private readonly string currency;

    public CurrencyTests(string currency) => this.currency = currency;

    [Test]
    public void Converts() => Assert.That(currency, Has.Length.EqualTo(3));
}

[TestFixture(typeof(int))]
public class GenericTests<T>
{
    [Test]
    public void Holds() => Assert.That(typeof(T), Is.Not.Null);
}

public class IgnoredTests
{
    [Test]
    [Ignore("broken")]
    public void Ignored() => Assert.Fail("ignored test ran");

    [Test]
    [Explicit]
    public void ExplicitOnly() => Assert.Pass();
}

public class PostingTests
{
    [Test]
    public void Posts() => Assert.That(Posts(2), Is.EqualTo(2));

    public int Posts(int amount) => amount;

    [Test]
    public void @event() => Assert.Pass();
}
