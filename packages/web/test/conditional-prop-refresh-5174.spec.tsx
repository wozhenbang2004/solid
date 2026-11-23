/**
 * @jsxImportSource @solidjs/web
 * @vitest-environment jsdom
 */
import { expect, test } from "vitest";
import { Loading, action, createSignal, flush, refresh } from "solid-js";
import { render } from "@solidjs/web";

async function tick() {
  for (let i = 0; i < 4; i++) {
    await new Promise(resolve => setTimeout(resolve, 0));
    flush();
  }
}

test.each(["ternary", "logical", "nested", "plain"] as const)(
  "a click reads the shown value through a %s prop during refresh",
  async variant => {
    const answers: ((value: number[]) => void)[] = [];
    const reads: unknown[] = [];
    let reask!: () => unknown;

    function Child(props: { count: number }) {
      const handleClick = () => {
        try {
          reads.push(props.count);
        } catch (error) {
          reads.push(error);
        }
      };
      return <button onClick={handleClick}>{props.count}</button>;
    }

    function App() {
      const [_$getOwner] = createSignal(true);
      const [answer] = createSignal<number[] | null>(
        () => new Promise<number[]>(resolve => answers.push(resolve)),
        { loadingValue: null }
      );
      reask = action(function* () {
        refresh(answer);
      });
      return (
        <Loading fallback="loading">
          {variant === "ternary" ? (
            <Child count={answer() != null ? answer()!.length : 0} />
          ) : variant === "logical" ? (
            <Child count={(answer() != null && answer()!.length) || 0} />
          ) : variant === "nested" ? (
            <Child count={_$getOwner() ? (answer() != null ? answer()!.length : 0) : -1} />
          ) : (
            <Child count={(answer() ?? []).length} />
          )}
        </Loading>
      );
    }

    const container = document.body.appendChild(document.createElement("div"));
    const dispose = render(() => <App />, container);
    try {
      await tick();
      expect(answers).toHaveLength(1);
      expect(container.querySelector("button")?.textContent).toBe("0");
      answers.shift()!([1, 2]);
      await tick();

      const button = container.querySelector("button")!;
      expect(button.textContent).toBe("2");
      button.click();
      expect(reads).toEqual([2]);

      reask();
      await tick();
      expect(answers).toHaveLength(1);
      expect(container.querySelector("button")).toBe(button);
      expect(button.textContent).toBe("2");
      button.click();
      expect(reads).toEqual([2, 2]);

      answers.shift()!([1, 2, 3]);
      await tick();
      expect(button.textContent).toBe("3");
      button.click();
      expect(reads).toEqual([2, 2, 3]);
    } finally {
      dispose();
      container.remove();
    }
  }
);

test("a ternary prop still suspends under Loading before the first answer", async () => {
  const { promise, resolve } = Promise.withResolvers<number[]>();
  const Child = (props: { count: number }) => <p>{props.count}</p>;
  function App() {
    const [answer] = createSignal(() => promise);
    return (
      <Loading fallback="loading">
        <Child count={answer().length > 0 ? answer().length : 0} />
      </Loading>
    );
  }

  const container = document.createElement("div");
  const dispose = render(() => <App />, container);
  try {
    expect(container.textContent).toBe("loading");
    resolve([1, 2]);
    await tick();
    expect(container.textContent).toBe("2");
  } finally {
    dispose();
  }
});

test("a conditional created outside a root keeps its mounted branch across truthy changes", () => {
  const [condition, setCondition] = createSignal(1);
  const element = (<div>{condition() ? <input /> : <span>hidden</span>}</div>) as HTMLDivElement;
  const input = element.querySelector("input")!;
  input.value = "typed";

  setCondition(2);
  flush();
  expect(element.firstChild).toBe(input);
  expect(input.value).toBe("typed");

  setCondition(0);
  flush();
  expect(element.textContent).toBe("hidden");
});
