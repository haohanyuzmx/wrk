# wrk压测--rust版

## 背景

wrk 每次使用都需要自己指定连接数，是一个比较麻烦的调优方式，没啥迹象可循

## 实现

主要抽象有两个，一个是负责执行异步任务的运行时；一个是负责压测的程序。

主要思想是自己实现一部分异步运行时；在该运行时管理的连接中，如果有事件就直接去处理事件，如果没有时间；就可以根据某些指标创建新的连接。

其中，这个指标可能是基于压测任务反馈（背压），也能是基于用户给定的一些知识。组合这些内容可以使用如卡尔曼滤波，或者pid调参。

（当然上面的内容还是画饼，目前只有一直创建新的连接