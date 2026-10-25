# Promoción a producción

Es el flujo **«Promoción a producción»** (ver «Flujos de despliegue»): en la pestaña **Flujos** del entorno de producción (en Entornos, **prod**) se ejecuta, se ve en qué paso va y se aprueba. Los pasos son los mismos de siempre:

```
1. Respaldo de prod ─► 2. Ensayo ─► 3. Cloud dev ─► 4. Esperar 30 min ─► 5. Promover
```

## 1. Respaldo de producción

- Botón **Pedir respaldo** (receta «Respaldo (export)» en prod). Sin aprobación: solo lee.
- Queda cifrado en esta Mac (`prod-<fecha>.zip.age`) con un número, por ejemplo #1.
- **Vale 2 horas** para promover.

## 2. Ensayo

- Botón **Pedir ensayo**. Usa el respaldo del paso 1 y el commit de `dev`.
- En un backend desechable de esta Mac: importa el respaldo, despliega el commit, corre los rellenos, revisa que no haya errores, registra el resultado y borra el desechable. **Producción no se toca.**
- Si el esquema nuevo no acepta los datos viejos, se pide desde la pestaña Trabajos de producción con `base_commit` (el commit a desplegar antes del import).

## 3. Cloud dev

- Botón **Pedir despliegue y rellenos**: pide «Desplegar en cloud dev» y, después, los tres rellenos.
- En **Aprobaciones**, aprueba **primero el despliegue**; si apruebas un relleno antes, corre sobre el código viejo.

## 4. Esperar 30 minutos

Cloud dev tiene que funcionar 30 minutos **desde el despliegue** sin errores de funciones. La página muestra cuánto falta. Mientras tanto, conviene probar la app contra cloud dev.

## 5. Promover

1. Botón **Pedir promoción** (se activa cuando todo lo anterior está en verde).
2. En **Aprobaciones**, escribe **«prod»**, **Aprobar** y confirma.
3. El trabajo vuelve a revisar todo, despliega Convex con la deploy key del Llavero, corre los rellenos, mira errores durante 5 minutos y despliega el frontend en Vercel. Si algo falla, se detiene ahí: sigue el log en la pestaña Trabajos.

## Las condiciones

| Condición | Cómo se cumple |
| --- | --- |
| CI en verde | ERP y el commit del submódulo (un repo con los workflows apagados cuenta como «sin CI configurado») |
| Mismo commit en cloud dev | Desplegado por el devtool (paso 3) |
| 30 min en cloud dev sin errores | Paso 4 |
| Respaldo de producción de menos de 2 h | Paso 1 |
| Ensayo con ese respaldo y ese commit | Paso 2 |

Si el respaldo venció (más de 2 h), repite los pasos 1 y 2: toman un par de minutos.
