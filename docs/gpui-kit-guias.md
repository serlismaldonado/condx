# Guías de GPUI Kit para Condx

Resumen de las guías oficiales que aplican a la app (`crates/app`). Las fuentes mandan:

- Coding Guides: <https://gpui-kit.com/docs/coding-guides/>
- View Cache: <https://gpui-kit.com/docs/view-cache/>
- También: Design Guides, Render, Context, Entity, SharedString (en el mismo sitio).

## Estado y vistas

- **`RenderOnce` para piezas sin estado propio** (como `Button`); **`Entity<T>` + `Render`** cuando
  hay estado retenido, tareas, suscripciones o hijos (como `DataView` de la pestaña Datos).
- **Crear las entidades una vez** (en el constructor o al abrir algo) y guardarlas en su dueño;
  nunca en `render`, porque cambia su identidad y se pierde el estado y la caché.
- **El estado va en el dueño más chico** que lo mantenga correcto (dominio en el modelo, lo
  transitorio en la vista que lo dibuja, lo diminuto en estado de elemento por clave).
- **`cx.notify()` después de cada cambio que se vea**, una sola vez por cambio coherente. Nunca
  notificar de forma incondicional desde `render` (bucle de redibujo).
- **Suscripciones** guardadas en el dueño (`_subscriptions: Vec<Subscription>`), no `detach` por
  defecto. Un hijo que apunta al padre lo hace con `WeakEntity`.
- **Trabajo asíncrono** desde un evento, el constructor o un método con nombre; no desde `render`.
  Estados explícitos (cargando, listo, falló) y descartar respuestas viejas (otra tabla, otro
  entorno) comparando su identidad.
- **Texto que se conserva en la UI: `SharedString`**; `String` solo para buffers que se editan.

## Caché de vistas (`Entity::cached`)

- `entity.cached(StyleRefinement::default().size_full())` reutiliza el render, el layout y el
  pintado de esa vista mientras siga limpia: los redibujos del padre no la reconstruyen.
- Necesita **una caja definida** (el padre le da el tamaño; no puede medir su contenido).
- **Todo lo que cambia lo que se ve tiene que notificar a esa vista**: sus lecturas, sus campos de
  texto (son entidades propias: suscribirse y notificar), un `Global` o un modelo que lea.
  Cambiar el idioma o el tema hace un refresco completo (`window.refresh()`), que salta la caché.
- Si la vista sale del árbol (otra pestaña) y vuelve, se dibuja de nuevo (la caché es del cuadro y
  de su posición); lo que se conserva es el estado de la entidad.
- Comprobarlo contando renders (`DEVTOOL_TRACE=1` imprime los de `DataView`): no debe dibujarse
  cuando cambia otra cosa, y sí cuando cambia lo suyo.
- Para colecciones largas, virtualizar (solo se construyen las filas visibles); son cosas distintas.

## Layout

- `h_flex` centra en el eje cruzado: una fila de columnas a alto completo lleva `items_stretch()`
  o columnas con `h_full()`.
- **Un solo dueño de cada scroll**; al hijo flexible que puede encogerse, `min_h_0()`/`min_w_0()`.
  Evitar scrolls anidados por accidente (la pestaña Datos apaga el scroll de página).
- Preferir el layout de GPUI a medir: medir solo cuando el layout no alcanza (popups, editores,
  virtualización) y nunca cambiar estado ajeno en cada `prepaint`.
- Escala en rem (`p_2()`, `gap_3()`, `w_56()`, `text_sm()`) en vez de `px(...)` en la UI de la app;
  `px` solo en límites físicos o medidas reales, documentados.

## Estilo y comportamiento

- Colores, radios y espaciados del tema (`cx.theme()` / tokens), sin hex ni rgb sueltos en la UI.
- Componer con los componentes semánticos (Button, Select, menús) antes de armar uno con divs.
- Un comando lógico, una sola acción o método (botón, menú y atajo llaman a lo mismo).
- `ElementId` estables a partir de datos del dominio (ids, nombres), nunca del índice de una lista
  que cambia ni de un texto traducido.
- `cx.bind_keys` antes de `cx.set_menus`; volver a llamar a `set_menus` si cambian (idioma).
- Nombres: vistas y entidades por concepto del producto; manejadores por intención
  (`open_table`, `load_page`); lectores booleanos `is_`/`has_`; índices `_ix`.
