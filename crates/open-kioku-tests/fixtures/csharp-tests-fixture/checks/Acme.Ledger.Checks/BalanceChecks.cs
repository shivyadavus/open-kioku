using NUnit.Framework;

namespace Acme.Ledger.Checks;

public class BalanceChecks
{
    [Test]
    public void BalanceStartsAtZero()
    {
        Assert.That(Opened().Balance, Is.EqualTo(0m));
    }

    private static Posting Opened() => new Posting();
}
