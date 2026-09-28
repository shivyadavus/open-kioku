package org.example.app;

import org.apache.commons.Widget;
import static org.mockito.Mockito.mockThing;

public class UsesLibrary {
    Object make() {
        Widget w = Widget.build();
        return mockThing();
    }
}
