import ReactMarkdown from "react-markdown";
import type { Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";

type MarkdownViewProps = {
  children: string;
  resolveImageUrl?: (url: string) => string;
};

export default function MarkdownView({ children, resolveImageUrl }: MarkdownViewProps) {
  const components: Components | undefined = resolveImageUrl
    ? {
        img({ node: _node, src, alt, ...props }) {
          const nextSrc = typeof src === "string" ? resolveImageUrl(src) : src;
          return <img {...props} alt={alt ?? ""} decoding="async" loading="lazy" src={nextSrc} />;
        },
      }
    : undefined;

  return (
    <ReactMarkdown components={components} remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]}>
      {children}
    </ReactMarkdown>
  );
}
