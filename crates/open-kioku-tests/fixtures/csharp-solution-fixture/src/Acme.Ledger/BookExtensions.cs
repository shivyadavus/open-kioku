namespace Acme.Ledger;

/// <summary>Reports over a book.</summary>
public static class BookExtensions
{
    public static int Balance(this Book book) => book.Audited;

    public static int Balance(this Book book, int floor) => floor;
}
