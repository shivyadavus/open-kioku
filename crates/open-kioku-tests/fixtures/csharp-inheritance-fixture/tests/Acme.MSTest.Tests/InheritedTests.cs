using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace Acme.MSTestChecks;

[TestClass]
public class BaseSuite
{
    [TestMethod]
    public void Inherits() => Assert.IsTrue(true);
}

[TestClass]
public class DerivedSuite : BaseSuite
{
}

public class Outer
{
    [TestClass]
    public class Inner
    {
        [TestMethod]
        public void Nested() => Assert.IsTrue(true);
    }
}

[TestClass]
public class IgnoredSuite
{
    [TestMethod]
    [Ignore]
    public void Ignored() => Assert.Fail("ignored test ran");
}

public partial class GuardTests
{
    [TestMethod]
    public void Rejects() => Assert.IsTrue(true);
}

public partial class UnmarkedTests
{
    [TestMethod]
    public void NeverRuns() => Assert.Fail("an unmarked class ran");
}
