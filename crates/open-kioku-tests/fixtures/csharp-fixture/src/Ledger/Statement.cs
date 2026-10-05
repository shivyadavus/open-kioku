namespace Acme.Ledger;

public record Statement(decimal Balance, string Currency);

public enum EntrySide
{
    Debit,
    Credit,
}

public interface IPostingRule
{
    bool Allows(decimal amount);
}
